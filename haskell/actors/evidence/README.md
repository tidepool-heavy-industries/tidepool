# Task-local evidence review

These portable Haskell modules are coordinator/reviewer helpers, not a public
Tidepool API or a service acceptance framework. They consume the current wave's
`plans/next/WaveContract.hs` unchanged. A reviewer uses `assessCheck` for a concrete
revision and retains each full `Checked`; a coordinator uses `compactReview` to
fold that survey without discarding its artifacts. Attach independently recorded
selected/executed counts; do not derive counts from exit code alone.

In the repository Nix environment (`nix develop` if not already entered):

```
ghci -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/Review.hs
```

With your retained `checks :: [Checked]` and exact candidate `revision :: Text`:

```haskell
map (assessCheck revision) checks
compactReview revision checks
```

`PassingEvidence` means only that supplied checks were recorded as firsthand execution and executed as
expected at the supplied revision with complete nonempty selections and evidence
references. It does not prove that the acceptance matrix is complete, artifacts
are authentic, the candidate was integrated, or the running binary matches the
source. Review those obligations separately. `ReproducedFailure` is diagnostic
evidence, never a passing product gate. Attributed runs/source review remain
retained but cannot satisfy firsthand execution. Unknown counts, blocked checks,
empty surveys and compile-only checks do not count as behavioral success.

Focused synthetic logic test (no hosted tools or backend execution):

```
runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/Tests.hs
```

Keep full typed deliveries alongside this projection. Existing requests retain
their captured result types; editing these files or rebinding a helper does not
update already captured closures or a child prefix. A restart requires reloading
source and recovering evidence; it does not preserve arbitrary resident closures.

## Retained survey consumer

`RetainedSurvey.hs` folds two actual historical logs from this wave, both for
`83733e706b5ce33c9cfdfedc75fb0f4e979a2415`, each recording 14/14 synthetic logic
checks. It takes the coordinator perspective: both implementer and reviewer runs
are attributed, not firsthand coordinator execution. It also displays the
implementer's firsthand view separately. Neither origin makes independence a
property of `DirectExecution`: reviewer origin and independence are a separate
evidence obligation. No new origin proof or runtime state is introduced.

```
runghc -Wall -Werror -ihaskell/actors/evidence -iplans/next haskell/actors/evidence/RetainedSurvey.hs
```

The historical survey is not evidence that the revised helper candidate passes.
Re-run checks at a new candidate and retain that revision separately; do not
retag the historical logs. The consumer prints historical revision and artifact
references before its fold. These `/tmp` logs are local retained evidence, not
portable repository fixtures. Reviewers must inspect them while available; the
pure helper does not open or authenticate evidence files.
