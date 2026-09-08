# Recursive implementation package improvement

The next iteration is [coordination-rsi.md](coordination-rsi.md). Its planner
handoff, fresh consultations and progress collection supersede the older execution
guidance below; this document retains the earlier checked package evidence.

Scope: next-wave authored package and shipped guidance. Keep the current original
root `.shoal`, running binaries and compiler selection frozen. Resume stopped waves
from committed artifacts and lane handoffs; do not revive old actors or assume
uncommitted state survived.

## Observed problem

The restart needed live steering because `componentLead` and `solTask` selected
fresh context and `atRef taskSource`, despite prose encouraging inherited recursive
implementation. Initial independent lane leads were reasonable fresh starts;
using the same defaults for every implementation child defeated the intended use.
Correction propagation also produced lengthy repeated planning/permission history.
This is observed friction, not measured proof of token savings from a replacement.

## Selected changes and verification

- Default implementation helpers to inherited context and `boundHead`; expose
  explicit source variants for original-root `projectHead` or exact `atRef` work.
  Keep context, source, model and lifetime separately composable. Reviews retain
  selected context and exact candidate source. No new Rust roles or workflow engine.
- Align examples and developer instructions: substantial leads own recursive
  implementation/integration; coordinator owns cross-tree joins; Astra reviews the
  first execution understanding and consequential changes. Small tasks stay small.
- Keep stable workflow in shared/role prompts. Task messages carry changing outcome,
  source provenance, ownership and checks. Preserve transport/incorporation semantics
  with concise evidence, not a narrative for every acknowledgment.
- Exercise model-free recipe success, review/repair, failure and next-wave freeze
  behavior. Extend recursive routing checks to retain a parent binding and use the
  parent's newer committed checkout despite an older task-source field. This proves
  resident context/source selection, not native provider transcript cache reuse.
- Check shipped guide examples/catalog after guide edits. Live behavior and cost
  improvements remain observations for the subsequent wave, not recipe assertions.

## Deployment and monitoring

Do not activate this candidate mid-wave. External RSI monitors actual first child
context/source, useful output, build reuse, disk growth and coordination noise.
If a stand-down is needed, ask owners to stop admitting new work, save coherent
partial commits and remaining failures/next steps, and preserve branch identities.
A later authorized boundary selects the new package and resumes those frontiers.
No agent cap or automatic termination of useful in-flight expert work is added.

## Checked candidate

- Candidate definition identity:
  `ade252940cca643da22934cf7e1cf0c0d8cae2d5c299874a0cb6fbdfb55ec6a0`.
- `shoal check --workspace examples/shoal-workspace --recipes`: 49 assertions
  passed (workbench 10, collaboration 20, routing 19), using the fixed restart
  runner and a separate persistent compiler. No model/provider workers launched
  by these checks. An initial cold-compiler check was stopped and replaced by
  this completed run.
- Shipped `actor_host::prompt_catalog` and
  `shared_api_guide_example_handles_success_and_unavailable`: 5 tests passed.
- Haskell modules and changed expression fixtures compile through those recipes;
  source formatting follows adjacent Haskell style; `git diff --check` passed.
- Current live wave has exercised explicit inherited/boundHead first implementation
  forks after steering. Provider-cache savings are not established by these checks.

A separate live monitoring finding: Nix Git-flake discovery can reject tracked
files through mounted working paths. Workers were steered to verify and use the
existing Nix toolchain, with explicit extractor-worker selection for tests, rather
than modifying the flake. This environment issue is not repaired by prompt changes.
