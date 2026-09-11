# Foreground execution proposal

Status: consolidated coordinator readback for the single initial planning
checkpoint. Production implementation remains pending planner release.

Baseline: `1c6f8816320e987cd61b031a90718662674c890f`.
The coordinator's first owning warm check, `bash scripts/dev-shell.sh cargo
check -p tidepool`, passed before the leads were admitted. The canonical sleep
interruption decision is commit
`2b6799a7810a47485e965413e48335d23bef5cb1`, incorporated in this line at
`56e6903eafbcb059f95aad06fcf21595c71b6659`.

The detailed lead proposals are:

- [resident sleep](sleep-proposal.md), proposed at
  `0eeb5074cd74ce7b1800e07890a826288d51b769`;
- [applications reconciliation](applications-execution.md), proposed at
  `f4c5d8e7aea6c3d93c81759fe81935be2e10dfd4`.

## Agreed seams and owners

The sleep lead owns the typed `Sleep` effect, reuse of `Tidepool.Duration`,
generated protocol/actor bridge, actor effect rows, resident suspension and
timer lifetime, the exact-evaluation cancellation primitive, handler behavior,
and shipped guidance. Its first shared seam is a compiling schema, checked
duration conversion, boundary request, and zero/short-delay actor path.

The applications lead owns selective Tidepool reconciliation from the preserved
R7 line, host recovery/custody, and the shared host cancel-and-settle transition.
It will preserve launch-main runtime/resource owners rather than merge the
168-file combined R7 tree. The native integrator owns reconciliation of
`d84cda697a8dac2842bec09dbd7562a3fab4c926` onto
`80e36633f515b03e11189e8516be21065e73335e`, including command Jobs, Bind,
immutable input, durable completion, and the semantic joins in
`host_dynamic_tools.rs` and `host_dynamic_tools/input_control.rs`.

Shared backend/host/native cancellation edits remain applications-owned. Sleep
provides the exact evaluation identity, cancellation result, and fixture
requirements. The coordinator owns the final Cargo/flake pin, fixture baseline,
combined host wiring, matched integration checks, release disposition, and the
real fifteen-minute smoke.

The binding interruption rule is exact: transport observation loss does not
cancel, complete, or permit replay. A delivered human/actor message queues
behind cancellation or terminal settlement of the exact suspended evaluation
before inference sees it. Queued inactive requests and collector/mailbox-only
data do not interrupt. Without terminal proof, the host neither claims
quiescence nor admits a conflicting evaluation.

## Released recursive frontiers

After planner release, sleep first lands its typed compiling seam. The lead then
may fork timer/lifetime evidence and native-wait acceptance while retaining
production cancellation/lifetime wiring, handler integration, guidance, and the
fold.

Applications first publishes the reconciled native Jobs/input-control seam and
the selective Tidepool host/recovery seam with owning warm checks. The native
integrator may then separate input-route identity/fence coverage from command
completion/cancellation/cleanup. The applications lead may separate recovery
adaptation from the matched scripted-provider consumer while retaining the
cross-owner cancel/expiry race and final pair.

Neither lane imports engine work, changes the running package, or edits the
canonical running `.shoal`.

## Verification agreement

Both lanes enumerate and compile every affected target, then run focused owning
tests for their changed boundaries. Sleep covers checked duration conversion,
zero and controlled fifteen-minute time, cancellation/expiry, retirement,
sibling progress, handler serialization, exact invocation identity, uncertain
terminal refusal, and transport loss without replay. Applications covers the
four named recovery cases, immutable input and stale-generation fences,
presentation/compaction, completion persistence, cancellation, retirement, Jobs
cleanup, and the matched full-TUI consumer.

The coordinator checks the resulting joins rather than replaying unchanged lane
evidence, runs required generated-fixture checks if serialization changes, and
runs one actual mock-provider fifteen-minute TUI sleep with one final result,
one suffix, no intermediate inference, prompt interruption, cancelled suffix
suppression, and subsequent usability. No workspace-wide suite or aarch64
execution is claimed.

## Planner decision

Approve the selective two-repository reconciliation and ownership map above,
including applications ownership of shared backend/host/native edits and sleep
ownership of the resident exact-evaluation primitive; otherwise identify the
specific retained application guarantee or outer sleep seam that requires a
different owner before implementation forks proceed.
