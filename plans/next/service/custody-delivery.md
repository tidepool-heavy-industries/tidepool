# Pre-bootstrap custody candidate after review repair

Tested production/test revision: `734c30fd2b98b74129cc140f9a86e6c27633551c`.
Review base: `45360ce1dd2990b1a9b516786645b545b71942ad`.
This is a repaired implementation candidate, not acceptance or mounted-service
verification. The initial implementation/evidence remains in Git and the
implementer's retained `target/custody-evidence/` directory.

## Contracts

The actor kernel acquires exact BindingTable custody before evaluating its
entry. Installation runs on a blocking task. Before any process submission,
the kernel/host's last shared lease settles the exact binding generation.
The host validates this binding rather than binding after bootstrap.

The review correctly rejected treating `kill_pane` success as process reaping:
absence or a pane outside the owned session also returns success. The repaired
interface has **no operation to clear the process-existence fence**. Once launch
may have external effects, the binding is conservatively retained, including
after successful pane removal. Process and binding cleanup receipts report
unconfirmed termination instead of completion. The earlier candidate's
`CustodyRelease` and `custodyRetainedByActor` outcome were removed before
integration; no new serialized cleanup case remains.

Cancellation required an actual lifecycle repair. Exact shutdown intent is
recorded in the existing retained-exit owner without publishing a terminal.
Bootstrap checks it before installation, after installation and at startup
continuation boundaries, before provider publication. Normal lifecycle cleanup
still publishes the terminal; a sender racing with this cleanup returns only
that observed terminal, never invented success. Linked-child shutdown uses the
same owning entry point. No new scheduler, registry or Haskell surface was added.

## Direct verification at the tested revision

- `NEXTEST_TEST_THREADS=1 just test-lib tidepool 'test(custody)'`: **9 executed,
  9 passed**. Includes real hosted Haskell with deterministic installation gates,
  two siblings, real `stopAgent` cancellation before/after binding, a real Haskell
  startup exception after an observed successful bind, and exact release with
  no provider publication. A private real tmux server proves missing/foreign
  panes cannot clear custody; the foreign pane remains present. No native model
  inference was used.
- `NEXTEST_TEST_THREADS=1 just test-lib tidepool-actor
  'test(shutdown_intent_does_not_publish_terminal_or_replace_first_request) |
  test(shutdown_releases_mailbox_custody_deferred_behind_external_work) |
  test(independent_roots_share_routing_but_not_supervision)'`: **3 executed,
  3 passed**. Intent is not terminal publication; mailbox/supervision regressions
  pass.
- `nix develop --command cargo build -p tidepool --bin shoal`: built, not launched.
- `cargo fmt --all -- --check` and `git diff --check`: passed. No Rust warnings;
  Nix reports ignored untrusted cache settings, without preventing the build.

Logs and SHA-256 identities: `target/custody-evidence/review-repair/` in the
implementer's worktree. Changed host documentation/research tests compiled but
were not individually executed. No broad battery or fixture-corpus update.

## Integration gate

The current tmux boundary cannot establish exact process termination. Consequently
**even ordinary successful tmux cleanup retains submitted actors' bindings**.
Worktree reuse after process submission is blocked until the service owner wires
an authoritative exact-process supervision/reap proof. Do not deploy this as a
claim of complete process cleanup or bypass its fence using pane absence.
Mounted native service/observer acceptance remains externally gated. The running
host and native dependency pin were not changed. The regression demonstrates a
failure class, not the precise cause of the historical authorization incident.
