# Notebook release completion

Integration owner: the direct Codex session. Baseline: coordinator `26f8f7494`
combined with overlay `73d7ec5a3`. Proceed linearly; use a bounded consultation
when a reduced reproducer exposes consequential uncertainty.

Recovery evidence is outside the checkout at
`/home/inanna/dev/notebook-takeover-recovery-20260912`: immutable branch tips,
tmux transcripts, source inventories, and patches copied from actor namespaces.
These patches are unverified inputs, not accepted implementations.

## Release checklist

- [x] Preserve modified tracked actor sources and combine committed baselines.
- [ ] Compiler-owned cell splitting, imports/prologue, and obsolete parser removal.
- [ ] Exact staged type transport, whole-cell rejection, and nominal recovery.
- [ ] Source-item receipts on rejection and accurate display/terminal accounting.
- [ ] Lookup usable names, real returned-name invocation, truthful hosted output.
- [ ] Status views and documentation lookup; no colon commands in cells.
- [ ] Automatic Generic and bounded structural rendering with opaque fields.
- [ ] Typed retained `last.more`, command paging, and per-cell display budget.
- [ ] Complete multi-step teaching corpus and runnable recipe migration.
- [ ] Focused acceptance, fixture boundary, recipe check, overlay admission and
      40-generation oracle on the combined revision; formatting and diff review.
- [ ] Commit, push, and land clean main while preserving unrelated live runs.

Stage 2 execution, field-specific continuations, and orchestration redesign are
deferred. A first-release feature is complete only when its real hosted consumer
passes, not when a neighboring unit test or a source-generation assertion passes.

## Verification evidence

## Next cell repair

The recovered notebook patch is 942 lines, not only the visible prologue edit.
It attempts to compile all staged statements against temporary declaration/value
interfaces before executing. Review and adapt it against the combined source:
`resident_actor.rs` already contains rejection changes, so the patch does not
apply wholesale. The other runtime/actor hunks apply in a check-only trial.

Do not copy `validate_declared_heads_in` unchanged: it identifies same-cell types
by occurrence name, so a reference to an older qualified type with the same name
would be mistaken for a new declaration. Preserve the complete nominal identity.
The prologue patch alone also does not establish matching flags during staged
execution: the existing declaration renderer hoists LANGUAGE text but not general
OPTIONS_GHC. Complete the compiler-owned prologue path across both phases.

## Completed checks

Initial combined baseline: `git merge codex/overlay-efficiency` from coordinator
`26f8f7494` completed without conflicts.

Lookup repair (combined baseline plus recovered five-file patch and qualified
operator fix):

- `bash scripts/dev-shell.sh bash -lc 'cd haskell && cabal test introspection-search-test && cabal build tidepool-extract-bin'`:
  passed the search suite, including compilation of returned qualified,
  ambiguous, and operator spellings; extractor binary built.
- `just test-lib tidepool-actor 'test(lookup_tool::tests::) | test(=resident_interactive::lifecycle_tests::hosted_lookup_isolates_type_failure_and_returns_next_cell_name)'`:
  6 passed, 141 skipped; the hosted case creates a real Response and applies the
  returned function spelling in a later cell.
- `just test-lib tidepool 'test(=host_dynamic_tools::tests::workbench_function_result_uses_endpoint_owned_text_boundary)'`:
  1 passed, 280 skipped; the endpoint returns the declared text representation.

The remaining lookup work includes raw-string transport and documentation topics;
these checks do not close the entire lookup release obligation.

Rust checks use `CARGO_TARGET_DIR=/home/inanna/dev/tidepool-overlay-efficiency/target`
and this checkout's explicitly selected Haskell worker. The test wrapper owns
the per-run compile daemon. Rerun affected cases after repairs rather than broad
batteries.
