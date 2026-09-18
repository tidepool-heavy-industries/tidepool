# Workbench lab: what one cell can do with cheap typed judgments

Driven live through `shoal proxy lab <cell.hs>` on 2026-09-17 against the toy
repo `~/dev/shoal-evals/tui-test-app` at two real failed builds:
`f726882` (five non-exhaustive match errors) and `4610b5e` (one clippy lint
promoted to an error by `-D warnings`, inside a test).

The cells that produced this ran here, in order, and have since been
promoted: the investigation function (`look` below) lives on as `investigate`
in `.shoal/Project/Investigate.hs`, and `routeFindings`, which turns its
answers into prepared next steps, lives on in `.shoal/Project/Review.hs`.

## The result

One call to `look` plus one call to `routeFindings`, costing two model requests
and about six git reads, produced this from nothing but the build output:

    must_edit:                 src/panels/status.rs:29, :49,
                               src/main.rs:136, :151, :158
    outside_ownership:         src/main.rs:136, :151, :158
    leave_alone:               src/app.rs:61 (the enum),
                               src/app.rs:64 (the new variant)
    already_covered_by_tests:  src/app.rs:293, :294
    ignore_compiler_suggestion: true

Every line is correct. Two of the locations, the test at `app.rs:293-294`, were
never named by the compiler; they came from a tree-wide search the cell ran
itself. The `outside_ownership` list is precisely the run-7 failure where a leaf
silently redesigned a shared type rather than reporting that it did not own the
files it needed.

## How it is built

Three sources of evidence, gathered by code:

1. the failed build output, split into diagnostics and grouped by shared cause;
2. `git grep` for the symbol named in the headline, which finds producers the
   compiler never reports because they compile fine;
3. `git show <oid>:<path>` for every file involved, sliced into excerpts around
   each location.

Two model requests, both pools with one question set per item:

- over diagnostic groups: is the shared location already correct, does each site
  need its own edit, is any site outside the owned paths, does the compiler's own
  suggestion insert a placeholder;
- over locations: must this be edited, does it already handle the symbol, is it a
  test, does it declare the symbol rather than consume it.

Then code routes. No model decides what to do next; it only answers what the
code could not read.

## What code decided without asking

Grouping. Five diagnostics sharing a headline and a `note: ... defined here`
target are one cause, which is a string comparison. The group is keyed on
content, so it does not depend on the order the compiler emitted them.
Ownership is prefix matching. Both were on my original list of things to ask the
model, and both were wrong to ask.

## Question wording

See `wording-ab.md`. The same packet with vague wording returned 0.41-0.42 on
three of five questions, which is no signal. Naming the narrowest deciding fact
moved the same judgments to 0.72/0.12, 0.98/0.02 across the two fixtures.

## Friction hit while doing this

Eight items were hit and reproduced while driving this; most already have
fixes landed. The still-open ones are carried forward in
[the top-level plans index](../README.md#carried-forward-one-liners).
