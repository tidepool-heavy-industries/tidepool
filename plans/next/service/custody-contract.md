# Pre-bootstrap custody implementation contract

The existing injected ForkWorkspaceAdmission owns installation in addition to
allocation. `install_custody(actor, worktree)` returns an opaque shared lease;
no new registry or identity issuer. Concrete host implementation holds the
existing BindingTable ActiveBinding receipt and releases that exact generation.
The default is explicitly unsupported, not success.

Before `ResidentBoot::Entry` can run any Haskell under the child principal,
install the exact allocated worktree binding. The kernel retains the lease for
its lifetime and clones it into LocalResidentInstallation, so host launch and
cleanup retain custody even if the actor exits first. No bootstrap or provider
start on installation failure. No lease is necessary without a worktree.

The host must consume/validate this already installed binding rather than bind
a second time. Preserve a clearly owned path for existing non-fork/root launch
consumers if source inspection shows they need it. Bind errors never broaden
access or trigger retry. Exact last-owner release must preserve checkout files
and surface release failure as bounded structured evidence. Ensure shutdown,
bootstrap failure and provider launch cancellation drop their owners.

Scaffold hole: trait signature only; the production implementation, entry gate,
lease propagation/validation, structured denial evidence and deterministic
regression tests are the implementation worker's obligation. Review should
challenge lifetime assumptions against actual shutdown ordering.
