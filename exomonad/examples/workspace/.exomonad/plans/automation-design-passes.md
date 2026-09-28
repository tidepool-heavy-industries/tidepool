# Automation design passes

## Assumption watcher: domain values and executable policy

The first draft required a fingerprint, a parallel text description, a projection
that manufactured both, and a pure relevance callback. This made the caller
maintain two representations and prevented a background semantic judgment.

The revised watcher observes a caller-selected domain value. The projection
chooses relevant events. Equality cheaply suppresses unchanged observations.
The policy receives `Change value` and returns a typed decision in an effectful
computation; deterministic policies are lifted with `pure`. The actor retains
both domain values, the decision, and any notification receipt for its 32 most
recent changes, plus a total count that exposes any evicted history. Ignored decisions
remain visible, and unresolved decisions ask the owner rather than meaning ignore.

`Project.AssumptionExamples` contains two compiled policy examples:

- `regression`: partially apply any ordered measure and an actionable message;
  reuse the same policy for failed checks, queued work, or other increasing costs.
- `semanticImpact`: partially apply the assignment and a domain renderer. Jev
  selects a decision constructor; code applies it to the explanation. There is no
  key string to decode into control flow. Rendering occurs at the judgment boundary.

These are ordinary functions over the same change value. They can be combined in
ordinary Haskell: handle known failures directly, ask Jev only for the remaining
changes, or ignore changes unrelated to the assignment. No new rule language is
needed. The watcher uses an explicit Jev-capable actor profile even for a pure
policy; it must be started by an actor whose ceiling permits those effects.

The runtime check exercises a real child's progress, repeated-value suppression,
a reported regression, a retained ignored improvement, and late attachment with
an unresolved policy. The semantic policy is compile-checked without calling Jev;
its judgment quality remains a wave experiment. The offline recipe driver
intentionally rejects notification sends as `NotificationUnavailable`; the check
asserts retention of that refusal, not successful delivery to a model.

## Decisions made across the other drafts

- Deferred probes retain actual commands and context rather than names requiring
  another lookup. A selected probe can go directly to execution. Ordinary
  `traverse` composes optional and failed selections.
- Collected test evidence retains its specification so diagnosis consumes the
  result without repeating the expectation and source.
- Preparation must continue from completion events. A foreground await with a
  30-second handoff cannot implement an automatic long-running preparation.
  Readiness produces the typed value that the continuation consumes.
- The review coordinator is the repair owner. Its reviewer returns findings;
  asking the reviewer to manage repair as well creates two competing owners.
- Retaining a handle in a handler's tentative state before another effect does
  not make that retention durable. Request admission callbacks and independently
  accepted retention messages must be used where provided.

These are drafts with concrete consumers. The next pass should use the compiled
notebook compositions to find further unnecessary context reconstruction, callbacks
that are too weak, and machinery that duplicates an existing owner. Trial exposure
starts only after integration and runtime checks, not when a module compiles.

## Combined source and notebook check

The integrated notebook namespace passed `Project.AutomationChecks.integration`
with 23 assertions: handoff projection, typed assumption policies, retained
interviews, terminal review readiness, preparation and bounded evidence recovery,
and command probes plus slow observation. Definitions fingerprint:
`cc25ee85b276ad10e5923dab21652c878ab8b3dfbee82226e7472929b69500e0`.
The matched runtime is Tidepool `a60dc0fa8`; the frozen validation executable SHA256
is `c6d7921dabb9ad4f6f9121d3ff03ba43cc69736934062a7253acca251e93eb4d`.
The retained local log is `/tmp/automation-integration-validation/check.log`.
No native model workers or providers ran. Notification checks establish attempted
sends and retained refusals from the offline host, not delivery to a model.

`SlowHandler` now permits effectful diagnosis without exposing private watcher
state. The owner raises that computation inside its handler. Handoff composition
uses existing candidate/review and check summaries, labels reports separately
from observations, and leaves remaining obligations explicit. Test/example modules
are imported by checks rather than added to every model's default namespace.
Browser workflow execution and the automatic repair coordinator have separate
pending gates; this combined check does not establish those workflows.
