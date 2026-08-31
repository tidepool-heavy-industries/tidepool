# Shared worktree-agent contract

The parcel prompt defines the task. This file defines how to execute and
report it. Follow both; the narrower parcel constraint wins when they differ.

## Stay inside the parcel

- Rebase onto the requested local dependency commits before editing.
- Read the repository and crate guidance named by the parcel prompt.
- Preserve unrelated work. Do not commit `prompt.md.tmp`.
- If a focused test exposes a real defect in the owning mechanism, fix that
  mechanism narrowly. Do not grow a second implementation around it.

Before adding an API or abstraction, search its intended production callers.
A helper used only by its new test is not evidence that the abstraction belongs
in production. Prefer one owning mechanism whose public entry point enforces
its invariants; callers should not need to remember a separate pre-check.

## Focused verification is still complete verification

The ban on broad batteries is a resource constraint, not permission to leave a
changed target uncompiled.

Before committing:

1. List every changed file and map it to its owning build or test target. If a
   public type, serialized shape, generated contract, or shared command changes,
   search the whole workspace for consumers and fixtures; the defining crate is
   not the complete target map.
2. Compile every changed or directly affected build/test target, even when
   executing it would be too expensive. For Rust integration tests, use the
   narrow equivalent of `cargo test -p PACKAGE --test TARGET --no-run` when
   execution is excluded. Compile one named downstream target rather than
   substituting an unrelated workspace-wide check.
3. Run the smallest exact tests that exercise each changed behavior and each
   important failure/recovery branch. A unit test in a neighboring target does
   not prove that a modified integration target compiles.
4. For extractor-backed behavior, first discover the repository's current
   toolchain environment and run one targeted test when resource limits permit.
   Do not silently treat a missing ambient variable as proof that the test is
   unavailable.
5. Run formatting or the narrow crate check needed for warnings, plus
   `git diff --check`.

Never run a prohibited workspace battery, broad suite, background test, or
parallel test command. If a necessary focused command unexpectedly fans out or
the host is saturated, stop it and report the exact unverified target.

Use precise result words:

- **passed**: the named test executed and passed;
- **compiled, not executed**: the target built via `--no-run` or equivalent;
- **skipped**: name the concrete blocker and the command that remains;
- **not tested**: only when no meaningful focused check exists, with a reason.

Never summarize a neighboring target's success as though it covered all files
you changed.

## Review before delivery

- Read the final diff as a reviewer, not merely as its author.
- Search for stale callers, duplicated policy, unused public surface, rendered
  string control flow, and comments that describe the old morphology.
- Check the negative path and recovery/cleanup behavior, not only the happy
  path.
- Confirm the worktree is clean after committing and report the commit hash,
  exact commands, and exact results.
