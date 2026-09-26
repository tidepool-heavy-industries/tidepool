# Supervisor synthetic Jev trials

2026-09-26, pre-wave18. The supervisor personally issued 16 live requests to
`/v1/systemone`, model alias `jev-latest`; responses identify `jev-1.13.0`.
Inputs are synthetic, with no repository secrets or transcripts. Raw inputs,
expected alternatives, distributions and usage are in `jev-synthetic-trials.json`.

These direct API trials exercise the review policy question/criteria, the reminder
question with a shortened independent-work policy, and diagnostic choice over two
supplied probes. They are semantic trials, not execution/authority tests or exact
serialized Haskell request snapshots.

- Review: six cases (local repair, outside ownership, vague findings, mixed scope,
  instruction-like findings, protocol change disguised as local edit). All top
  choices matched the expected repair/escalation/insufficient route.
- Reminders: six cases (ready independent work, silence, dependency, already-owned
  work, instruction-like evidence, exhausted resources). Five top choices matched.
  Already-owned work selected suggest at mass 0.51/confidence 0.26 versus skip
  0.43. The Haskell careful policy's minimum mass 0.55 rejects this result; do not
  call it a correct raw classification or claim it demonstrates reliable nudging.
- Diagnostics: four cases (missing assets, dirty source, absent evidence,
  instruction-like stderr). All top choices matched; unknown cases can still
  be rejected by settlement policy rather than acted on.

Actual Haskell `Project.WorkflowReminderChecks.semanticCases` subsequently passed
3/3 live assertions: routine review relay recognized; shared-design dispute
excluded; actor silence does not establish fork readiness. The model-free actor
recipe uses the configured real Jev backend while native coding inference is
mocked. Definitions hash `bf0c3a4959b1b15c9dddbb719796a3830588f9115d4b0b5512181cd44b7daa39`.
Deterministic reminder mechanics separately passed 6/6.

Limits: one small hand-authored sample, no statistical efficacy claim, no measured
round savings. Baseline ambiguous-owner trials remain pending a source correction:
per-item questions must identify their subject; question IDs alone are not model
input. Integrated-source and production-consumer checks remain separate gates.
