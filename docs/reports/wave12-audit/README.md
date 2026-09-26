# Wave 12 multi-arm audit

Run: `7d0bd907-1640-49b1-a952-f98005ca6e2d` in exomonad-harness.
Audits are observations of an ongoing run, not its final verdict. Each report
states its cutoff and coverage. Source traces remain private local artifacts.

## Arms

- confusion: orientation, type/parser errors, wrong assumptions, recovery.
- abstraction: missed compositions and demonstrated useful abstractions.
- coordination: delegation, event routing, review boundaries, runtime costs.

Each arm writes `<arm>.md` plus `<arm>.json`. JSON is an object with `arm`,
`run_id`, `cutoff_utc`, `coverage`, `findings`, `unknowns`. Each finding has:
`id`, `kind` (failure/opportunity/success), `actor`, `operation`, `evidence`
(array of file/timestamp/call-id references), `observed`, `hypothesis`,
`confidence`, `impact`, `owner`, `proposed_action`, `validation`, `pipeline`
(array drawn from prompt, api, runtime, environment, experiment, interview),
`related_findings`. Use null where a causal hypothesis is not established.

Report all episodes in the arm's sampled scope, including counterexamples and
successful recoveries. Distinguish covered evidence from unknown or inaccessible
history. Group recurrence without hiding individual episode references. Include
counts only with a denominator and avoid equating first edit with first useful
work. Missed abstractions are counterfactuals requiring consumer/reuse evidence.

The supervisor reconciles duplicates and produces the next-stage work queue.
An audit recommendation is not automatically approved implementation or a new
constraint on the live actors. No audit arm steers the active run.
