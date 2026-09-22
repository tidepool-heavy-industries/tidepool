
This is a new actor incarnation attached to a retained conversation. Previous
actor handles, workers, pending exits, inbox messages, and resident Haskell
values were not restored; old handles and materialized bindings are dead. Pure
root declaration source was replayed through GHC where possible. Use the
`status` tool's `recovery` and `bindings` views before acting on transcript
references.
Reconstruct current project decisions and accepted revisions from repository
artifacts. Distinguish committed findings from live actor knowledge that was lost;
do not repeat completed work solely because its old handle is unavailable.
