# Pre-bootstrap custody integration contract

The existing injected ForkWorkspaceAdmission owns installation in addition to
allocation. `install_custody(actor, worktree)` returns an opaque shared lease;
no new registry or identity issuer. Concrete host implementation holds the
existing BindingTable ActiveBinding receipt. Installation is required, with no
default successful or unsupported implementation remaining.

Before child entry evaluates any Haskell under its principal, the kernel
installs exact allocated worktree custody. Kernel and LocalResidentInstallation
share the lease. Host validates the preexisting binding instead of binding late.
No bootstrap or provider publication follows a failed installation or observed
shutdown intent. Shutdown intent is retained separately from published terminal
in the existing exit owner and checked at bootstrap safe boundaries.

Before process submission, last-owner Drop releases only the owned generation,
retaining checkout files. After submission may have occurred, the current tmux
boundary cannot prove exact process reaping: its missing/foreign-pane success
and ordinary pane deletion are insufficient evidence. The fence is therefore
irreversible in this staged slice and cleanup explicitly reports unconfirmed
binding release. There is no new serialized cleanup outcome variant.

PRODUCT GATE: do not deploy this slice as complete custody support. Ordinary
tmux-launched actors retain bindings after cleanup. The replacing service
supervisor must supply authoritative exact-process termination proof and a
reviewed release transition before release/reuse can be accepted. Actor terminal
publication alone is not that proof. Do not weaken the fence to pane absence.

Independent review and evidence: custody-review.md. This corrects the initial
scaffold's unconditional last-owner release assumption. The custody implementer
and reviewer are retained for service integration repairs.
