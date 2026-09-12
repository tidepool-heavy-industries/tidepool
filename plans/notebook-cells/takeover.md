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
- [x] Compiler-owned cell splitting, imports/prologue, and obsolete parser removal.
- [x] Exact staged type transport, whole-cell rejection, and nominal recovery.
- [x] Source-item receipts on rejection and accurate expression/terminal accounting.
- [x] Lookup usable names, real returned-name invocation, truthful hosted output.
- [x] Status views and documentation lookup; no colon commands in cells.
- [x] Automatic Generic and bounded structural rendering with opaque fields.
- [x] Typed retained `cellDisplay.more`, command paging, and per-cell display budget.
- [x] Complete multi-step teaching corpus and runnable recipe migration.
- [x] Focused acceptance, fixture boundary, package compilation, overlay admission
      and 40-generation oracle on the combined revision; formatting and diff review.
- [x] Once implementation is ready, fresh Sol reviews of prompting and projected
      worker UX, plus the integration owner's own read-through. Review the actual
      shipped context and runnable examples; repair findings before landing.

Landing uses a fast-forward of main and a normal push; unrelated live-run state
remains outside this change.

Stage 2 execution, field-specific continuations, and orchestration redesign are
deferred. A first-release feature is complete only when its real hosted consumer
passes, not when a neighboring unit test or a source-generation assertion passes.

## Architecture review and next repair

[Architecture findings](architecture-review.md) track the review against the real
hosted consumer. Cell preparation now claims exact identities and leases compiled
dependencies before execution; these are structural repairs to the recovered code.

Current integration: the resident parser and unprepared execution bypass are
removed. Staged declarations adopt their validated artifact under session/scope/
generation fences. Hosted text, custom/automatic display, lexical-prefix `cellDisplay`,
and command paging checks pass. Compiler eligibility and inherited-child checks,
the combined fixture and overlay gates, and two fresh model-facing UX reviews
are complete. The full recipe run was stopped under the owner’s spot-check
preference; its partial evidence and limits are recorded below.

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

Raw-string transport and documentation-topic checks completed subsequently, as
recorded below.

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

The final integration results below supplement these early focused checks.

Source-plan rejection: the hosted multiple-errors fixture passed (1 test, 23s).
Its receipt retains every source item, maps both errors to their original cell
lines, and confirms no statement binding was installed.

Hosted tool transport: raw-string and batched lookup checks passed. Status parser
and registration checks passed (4 tests), the hosted lookup/status case exercised
all six views, default selection, and invalid input (1 test), and downstream host
registration passed (1 test). Resident colon entry points have been removed; standalone REPL command policy remains.

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

Display integration on the current working revision:

- Hosted lexical/prefix `cellDisplay`: passed, including runtime-prefix publication and
  preservation through later typecheck rejection.
- Hosted large Text: passed; exact 8192/1808-character pages cover 10000 characters.
- Hosted generated/custom Display: passed; function field opaque, explicit
  renderer preserved.
- Hosted command paging: passed; 10000 bytes recovered with one backend execution.
- Staged declaration adoption: two real-GHC tests passed for recovery recording,
  stale rejection, and safe discard after adoption.

The final combined overlay oracle is
`actor_host::overlay_resource::tests::forty_generations_share_artifacts_and_preserve_whiteouts`
in the tidepool library. Run it after notebook integration; stop and retain the
failing revision if it fails. Earlier overlay-branch results do not replace it.

Final integration checks on the combined working revision:

- `just fixtures-update`: regenerated the canonical corpus; only its source
  fingerprint changed, with no CBOR or ask-site delta.
- `just fixtures-check`: 217 semantic fixture tests passed.
- `forty_generations_share_artifacts_and_preserve_whiteouts`: passed.
- `ordinary_admission_captures_root_before_startup_and_busy_uses_head`: passed.
- Both validated-adoption/stale-sibling-artifact unit tests passed.
- Successor recovery with old and shadowing nominal types passed.
- `cargo check -p tidepool-repl`, its transport DTO unit test, and
  `cargo build -p tidepool --bin shoal` passed.
- Post-rename hosted paging, previous-cell/prefix semantics, Console/result
  budget sharing, and custom/generated displays passed (four tests).
- An inherited `unfold` child successfully declared a function of its fresh
  `cellDisplay`, used the default `Set` alias, and called its inherited closure
  over the parent's page.

Two independent fresh Sol UX reviews are complete. Repairs remove stale `:doc`
responses, unnecessary tuple wrappers and guaranteed-pending polling, correct
exact-revision types and request arguments in walkthroughs, and distinguish
inherited from selected-context skill knowledge. The migrated quiet-observation check also passed: exact retained response fields,
observation expiry, and effects-once behavior. The large design/review case with
only a `WatchReady (True)` rendering expectation change was not rerun. The
full package recipe run was intentionally stopped after roughly 15 minutes
in its first routing scenario, with no reported assertion failure. Workspace
definitions and the recipe program compiled; reached checks passed for amendment
deltas, inherited reasoning, current-parent source, and actual candidate source
evidence. This is partial recipe evidence, not a passing `--recipes` suite.

Final import consistency: `print` and `cellDisplay` share the actor source imports
used by both cell checking and declaration staging. A declared `emit x = print x`
helper passed the hosted shared Console/result-budget case. The prompt catalog
check passed (one test), and the changed pragma consistency target passed all
three tests.

The final Shoal binary rebuilt after the shared-import fix. No new provider-backed
dogfood run was started, so efficacy and usage changes remain unmeasured.
