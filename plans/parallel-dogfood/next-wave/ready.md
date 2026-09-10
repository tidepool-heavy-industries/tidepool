# Next launch prerequisites

No next-run selection is declared ready by this document. The release runner and
aggregate swarm resource placement are being finalized on main. Launch also needs
operator activation of the host configuration and explicit authorization.

Before launch, record:

- Accepted main and matched native runtime revisions, immutable runner and package.
- Verified active swarm memory/swap limits and working command-resource service.
- Preserved R7 sources from [resume.md](resume.md), with applications/native
  continuations reconciled on the selected baselines. Old prepared R6 restart
  branches are historical, not the next source selection.
- The applications [wrap-up assignment](applications.md) and the engine's actual
  remaining frontier from its R7 handoff. No shared-server migration implementation.

The launch record is authoritative for executable/package hashes and any prepared
continuations. Keep running tools frozen and product candidate tools separately
selected. This file does not authorize starting agents.
