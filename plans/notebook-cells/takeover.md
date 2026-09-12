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
- [x] Source-item receipts on rejection and accurate expression/terminal accounting.
- [x] Lookup usable names, real returned-name invocation, truthful hosted output.
- [ ] Status views and documentation lookup; no colon commands in cells.
- [ ] Automatic Generic and bounded structural rendering with opaque fields.
- [ ] Typed retained `last.more`, command paging, and per-cell display budget.
- [ ] Complete multi-step teaching corpus and runnable recipe migration.
- [ ] Focused acceptance, fixture boundary, recipe check, overlay admission and
      40-generation oracle on the combined revision; formatting and diff review.
- [ ] Once implementation is ready, fresh Sol reviews of prompting and projected
      worker UX, plus the integration owner's own read-through. Review the actual
      shipped context and runnable examples; repair findings before landing.
- [ ] Commit, push, and land clean main while preserving unrelated live runs.

Stage 2 execution, field-specific continuations, and orchestration redesign are
deferred. A first-release feature is complete only when its real hosted consumer
passes, not when a neighboring unit test or a source-generation assertion passes.

## Architecture review and next repair

[Architecture findings](architecture-review.md) track the review against the real
hosted consumer. Cell preparation now claims exact identities and leases compiled
dependencies before execution; these are structural repairs to the recovered code.

Next: replace the remaining resident line/block parser, carry a single GHC-owned
source/prologue plan through checking and execution, and preserve source-item
receipts when checking rejects the cell. Do not adopt the recovered prologue patch
unchanged: it moves pragma text but does not establish matching compiler flags
across every phase.

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

Structural cell preparation spot checks:

- Hosted prefix/rejection/shadowing fixture: passed (1 test, 49s).
- Hosted inferred-Response plus retained-old-type fixtures: passed (2 tests, 77s),
  including a stored action used in a later cell and qualified Map imports.
- Hosted terminal reply fixture: passed (68s alongside the earlier response
  trial; the response trial used an incorrect explicit test row, corrected).
- Hosted observation-lease suffix fixture: passed (1 test, 49s), after nine
  preceding displays expire its dependency's public name.
- Binding-owner lease/observation/retirement selection: 4 passed, 117 skipped.
- Runtime compile-view and dropped-preparation cleanup selection: 4 passed,
  157 skipped.
- Worktree-local Haskell worker rebuilt; Rust formatting and diff check passed.

The full fixture boundary, recipe check, final combined overlay tests, and broader
notebook release checks remain outstanding. These focused checks do not close the
release checklist.

Source-plan rejection: the hosted multiple-errors fixture passed (1 test, 23s).
Its receipt retains every source item, maps both errors to their original cell
lines, and confirms no statement binding was installed.

Hosted tool transport: raw-string and batched lookup checks passed. Status parser
and registration checks passed (4 tests), the hosted lookup/status case exercised
all six views, default selection, and invalid input (1 test), and downstream host
registration passed (1 test). Colon entry points still await removal.

Compiler-owned prologue integration: the splitter suite passed, including
multiline imports, comments, option negation, and located late-pragma rejection;
the worker rebuilt without warnings. Runtime and actor libraries compiled. All
27 declaration-renderer tests passed. Hosted prologue, multiple-errors rejection,
and retained-old-nominal-type checks passed together (3 tests, 67s). The legacy
harness's import-view spot check passed (1 test); its caller now consumes the same
declaration receipt rather than prefixing authored source with import text.

Documentation lookup: the parser spot check and hosted mixed documentation/name
batch passed (1 test each). `doc` lists existing catalog topics and `doc <topic>`
returns one topic; a missing topic does not hide subsequent batch results.
