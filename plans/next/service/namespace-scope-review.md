# Namespace scope independent review

Reviewed implementation `7770bdf7bbd5a21a7f5ec9bfbf7c267f8c1a4db2`, repairing
`31aab9b1e09bae401b7a844acb58682fb6568876`, against namespace-scope-contract.md.
Accepted for parent integration as an opt-in local process owner, not deployment
or actor custody-release acceptance. Existing wrap/tmux composition is unchanged.

## Decisive review repairs

- Initial gate tests killed init immediately after dropping writers, potentially
  masking accidental EOF release. Tests now observe the live blocked init for
  250 ms without signaling, including after killing and waiting the monitor.
  A test-only mutation removes sync self-hold and positively detects unsolicited
  payload startup from EOF. This is bounded empirical evidence, not a universal
  scheduler proof; the fd ownership/source invariant supplies the design argument.
- Forced monitor death is also exercised after detached double-fork/setsid
  descendant readiness, requiring exact-init drain and descendant socket EOF.
  Monitor status alone does not construct a receipt.
- Termination intent enters Stopping before signaling, so timeout cannot leave
  release_command authorized; explicit cleanup retry retains the witness.
- Public entry-point docs expose trusted bwrap/default-init and kernel exit-order
  prerequisites. Absolute path and uname strings are not binary attestations.

## Independent checks

At exact repaired revision: `nix develop --command cargo test -p tidepool-node
--lib process_boundary:: -- --test-threads=1` executed 17 tests, all passed, none
ignored (12 service scope plus 5 legacy boundary). Full node lib test target
compiled. `nix develop --command cargo build -p tidepool-node --example
service_scope` compiled the consumer. Running that example with the tested Nix
bwrap and private canary workspace exited 0, reporting monitor status 137 after
intentional termination. It proves cleanup, not payload completion. Formatting
check and git diff --check passed.

Reviewer retained evidence directory:
`/home/inanna/.cache/tidepool/shoal/actor-worktrees/f79d0ae899951577b2fea06ed8f088919d48f5f01e791513abffefa7fbf9fe02/worktrees/wt-3659859f-8e39-4a9b-ab07-6840bc6af1b6/.shoal/evidence/scope-review/`

Files: repaired-tests.log, repaired-example-build.log, repaired-example-run.log,
repaired-hashes.txt, format.log; candidate-* logs preserve initial independent
passing evidence before repairs. Implementation and repair logs remain retained
in implementer worktree wt-a41d6e87-df81-40fc-a409-b666ab3fb39a.

## Source audit and limits

Inspected proc-directory -> pidfd -> fresh relative status ordering, strict
monitor parent/PID namespace/inode checks, private CLOEXEC fds above stdio,
blocking gate and default bwrap init sync-fd keep-set, phase/error retention,
separate pidfd readiness and Child wait, and representative consumer. Only
validated init witnesses are signaled; Drop is best effort without a receipt.

Pre-pin uncertainty can strand blocked init; no successful cleanup is claimed.
Actual namespace-permission denial and nested proc PID views are not executed;
non-proc unsupported and stale retained-directory paths are exercised. Numeric
PID reuse is not forced. Kernel uname is 6.12.63; exact kernel binary identity
and ordering are not attested by tests. Local namespace descendants do not cover
external daemons/remotes or host-side Haskell effects. Production host wiring,
public reexports and custody policy remain parent-owned and untested here.
