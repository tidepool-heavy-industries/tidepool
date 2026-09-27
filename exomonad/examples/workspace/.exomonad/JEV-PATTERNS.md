# Composable Jev patterns: experimental examples

These are small question constructors and typed interpreters. Ordinary Haskell
owns effects, limits, handles and continuation. Start with the closest authored
client; change its criteria, evidence or rendering for the work at hand. Do not
introduce a Jev call where an existing receipt or deterministic rule answers it.

| Need | Pattern | Authored clients |
| --- | --- | --- |
| Diagnose a failed check; offer a bounded next step | `Project.CommandTriagePattern` | `Project.CommandTriageExamples`: check failure and package fetch |
| Pick one useful source while retaining its typed value | `Project.EvidencePattern` | `Project.EvidencePatternExamples`: diagnostic location and review source |
| Compare an update with explicitly incorporated facts | `Project.CoordinationPattern` | `Project.CoordinationPatternExamples`: review update and consumer checkpoint |

## Composition that matters

The constructors return ordinary Jev questions; append local questions with `:&`.
Use the examples' state and packet values with either `J.ask` or `J.request`.
Keep the full response, then interpret it with an explicit policy. Service error,
policy doubt and a settled insufficient-evidence answer are distinct outcomes.
None of these modules merges, retries, acknowledges delivery or suppresses a notice.

For several questions about the same evidence, put structured evidence in shared
state once. Let candidate wording refer to it through `J.field`. Do not assume a
sibling question can use excerpts buried in another question's alternatives. The
evidence helper accepts a complete candidate renderer: `describeEvidence` is the
standalone default; the compiled examples show shorter shared-state references.
A selected candidate carries its original typed payload; no string-key lookup is
needed to recover it.

Command triage batches a possible transience question only when the caller allows
another attempt. It consumes that answer only on the tooling branch. The returned
follow-up retains the policy type. An offered repeat is advice: the existing
command owner must still consume its budget and respect a required delay.

Update comparison checks missing task/source/evidence before asking Jev. Its
incorporation references describe what the owner actually incorporated; receiving
a message or observing silence does not establish that fact. Keep decisions
inspectable while evaluating this pattern; automatic notice suppression is not
part of the example.

## Try and evaluate

These modules and their clients have compiled. The construction recipes are
`Project.CommandTriageChecks.construction`,
`Project.EvidencePatternChecks.construction`, and
`Project.CoordinationPatternChecks.construction`. They use no provider. Live
semantic probes are a separate operator exercise: exact Haskell requests, live
responses, then typed decoding and policy replay. Recipe sessions do not install
a Jev backend.

Try the examples on a recurring task before making a framework. Report useful
branches, uncertainty, wrong decisions, overrides, packet usage and model turns
avoided. Synthetic cases establish neither reliability nor savings on a wave.

## One bounded notification trial

`Project.NotificationTrial.notificationEpisode` projects an explicit retained
collector snapshot and event index. Pass the notice policy used for that event;
a later policy change does not rewrite history. Submit the episode to a separate
`ReminderTrial`, inspect its retained judgment, and finish that trial when done.
The collector keeps its ordinary sink. There is no automatic suppression or
background watchdog. A trial actor failure therefore cannot interrupt collection.
The deterministic recipes exercise retention and stopped-trial isolation; semantic
quality on actual work remains an experiment.
