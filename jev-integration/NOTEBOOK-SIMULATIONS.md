# Linked notebook simulations

Actual Jev calls, synthetic command outputs. No shell command or actor notification
is executed. The selected alternative supplies the next fixture/continuation;
unselected branches are never added to the next request. Each run is capped at
three calls. HTTP/interpretation failure stops the chain, without retries.

Expectations recorded before running:

| Simulation | Expected selection path | Expected terminal result |
| --- | --- | --- |
| failure | d2 → offset_probe → supported | Stale retained offset supported by the diagnostic observation |
| source | h2 → e2 → supported | Exact publication source witnesses cancellation gate |
| context | c1 → deliver | Existing accepted dedup contract answers worker question |
| swarm | ab → joint → consult | Minimal joint evidence exposes missing shared semantics |

Wrong early choices have explicit `need_evidence` terminals or lead to weaker
observations. The failure benchmark alternative returns timing data only, so its
final assessment should not support the stale-offset hypothesis. The swarm's final
assessment receives only the actually selected packet, not the omitted evidence
from prior calls. Simulations exercise selection, not threshold calibration.

Run with key in the existing environment:

```sh
target/debug/jev-integration simulate failure --output-dir jev-integration/evidence/notebook-failure-001
```

Other names: `source`, `context`, `swarm`. Output directory must be new; its parent
must exist. Every step uses the existing private, bounded, credential-redacting
capture path and source fingerprint. `summary.json` retains selected path and
synthetic observations. Partial attempts retain step evidence even if interrupted;
an empty/incomplete summary is not a completed run. Files are created mode 0600.

This implements the notebook microprogram mockups (an uncompiled sketch, now
git history) as Rust research state machines, not as an implemented Haskell
DSL or actual notebook execution. Helpers, budgets and transitions are
explicitly authored.

## Results — 2026-09-16

All four paths matched the predictions on their first run. Eleven requests returned
HTTP 200 from `jev-1.13.0`, with no interpretation findings or retries.

| Simulation | Actual path (selected-option probability) | Calls | Sum of API milliseconds | Input / output tokens |
| --- | --- | --- | --- | --- |
| Failure | d2 (1.00) → offset_probe (0.99) → supported (0.94) | 3 | 426 | 1,758 / 121 |
| Source | h2 (1.00) → e2 (1.00) → supported (0.98) | 3 | 507 | 1,828 / 107 |
| Context | c1 (0.96) → deliver (0.98) | 2 | 334 | 1,197 / 80 |
| Swarm | ab (1.00) → joint (0.98) → consult (0.95) | 3 | 428 | 1,854 / 118 |

Totals: 6,637 input tokens, 426 output tokens. API times are summed recorded
request elapsed times, not complete notebook wall time; simulated commands take
no real command time. Real command, interpreter, queue, and model overheads remain
unmeasured. Per-step probabilities must not be multiplied into workflow confidence.

The failure chain inspected the useful diagnostic, selected the discriminating
probe, and supported the stale-offset hypothesis from its concrete observations.
The source chain bypassed telemetry for the actual publication path. The context
chain reused the accepted contract instead of requesting a new semantic decision.
The swarm chain isolated interacting publications, selected their joint contract
evidence, and requested consultation from that packet alone.

All choices were made by Jev, not overwritten with expected answers. Subsequent
fixture selection is a deterministic lookup in the exact submitted candidate map.
These are small, obvious examples; candidate descriptions help substantially.
The wrong-branch paths exist and are bounded, but were not selected in live runs.
No robustness to large logs, ambiguous code, conflicting evidence, or different
model versions is established. Final assessments are currently Choice-only
projections of the richer mock `Assess` schema, not overlapping-judgment tests.

Each `evidence/notebook-<scenario>-001/` contains `step-N.json` captures and
`summary.json`. The output files remain private and ignored. The shared transport
and evidence writer serve both standalone probes and linked simulations.

Verification: package build passed; 13 unit tests passed, including the new check
that every fixture path (including wrong choices) terminates within three calls;
clippy with warnings denied, package formatting and `git diff --check` passed.
No workspace or Haskell engine tests were run for this Rust-only research client.
The TypeSafe skill's dependent-call guidance informed separate requests after
new observations; there is no same-request dependency or batching mechanism.
