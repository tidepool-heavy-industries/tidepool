# Sync: TypeSafe pattern exploration in a live Shoal workbench, 2026-09-17

## Setup

A headless Shoal session named `lab` runs against the toy repo
`~/dev/shoal-evals/tui-test-app` with a cheap idle root. All work is driven from
the shell with `shoal proxy lab <cell.hs>`, which submits one Haskell cell into
the session as a root-equivalent operator workbench. Bindings and declarations
accumulate across cells, so the session is a persistent typed scratchpad. Cells
and notes are in `plans/jev-lab/` in the `tidepool-jev` worktree.

Fixtures are two real failed builds of the toy repo, kept as commits:
`f726882`, which fails with five `error[E0004]: non-exhaustive patterns:
app::ActivePanel::Tags not covered`; and `4610b5e`, which fails with one clippy
`field_reassign_with_default` promoted to an error by `-D warnings`, inside a
test, whose note points one line above the primary site.

## The main result

One authored function, `look`, plus a routing function, `routeFindings`, take a
failed build and produce prepared next steps. Cost: two model requests and about
six git reads. Output on fixture A, with owned paths `["src/panels/"]`:

    must_edit:                  src/panels/status.rs:29, :49,
                                src/main.rs:136, :151, :158
    outside_ownership:          src/main.rs:136, :151, :158
    leave_alone:                src/app.rs:61 (the enum),
                                src/app.rs:64 (the new variant)
    already_covered_by_tests:   src/app.rs:293, :294
    ignore_compiler_suggestion: true

Every line is correct. Two locations, the test at `app.rs:293-294`, were never
named by the compiler; the cell found them by running its own tree-wide search.
The `outside_ownership` list is exactly the run-7 failure in which a leaf
silently redesigned a shared type instead of reporting that it did not own the
files it needed.

Underlying per-location judgments, nine locations, one request:

| location | must_change | declares | is_test |
|---|---|---|---|
| the enum definition | 0.33 | 0.90 | 0.06 |
| five compiler-reported match sites | 0.82 to 0.86 | 0.05 to 0.08 | 0.03 to 0.04 |
| the variant declaration | 0.27 | 0.96 | 0.05 |
| two test lines found by search | 0.26, 0.20 | 0.08, 0.11 | 0.98, 0.98 |

Three axes, nine items, clean separation on all three.

## What code decided without asking a model

Grouping. Five diagnostics sharing a headline and an identical
`note: ... defined here` target are one cause, which is a string comparison. I
had planned to ask the model to group them. The group key is content, not
position, so it is stable under any reordering of the compiler's output.

Ownership is prefix matching. A separate reading of the entity-alignment and
semantic-search recipes reached the same verdict independently: our existing
pure-code ownership check is already correct and a model there would only add
noise.

Both of these were on my original list of questions to ask. Both were wrong to
ask. The saved effort went into the tree-wide search and the per-location
judgments, which are things code cannot do.

## Question wording, measured

Same fixtures, same packet shape, same model, one request each. Only wording
changed.

Vague, on fixture A: "should the repair edit the shared location rather than
each site" 0.42; "would a single edit resolve every site" 0.42; "is the
compiler's suggested fix right" 0.41. Three of five questions returned no signal
at all.

Narrow, on fixtures A and B: "is the code at the shared location already
correct, so the repair must change the listed sites instead" 0.67 / 0.28; "does
each listed site need its own separate edit, because the sites are in different
functions or files" 0.72 / 0.12; "is at least one site in a file outside the
owned paths" 0.96 / 0.89; "does the compiler's suggested fix insert a
placeholder such as todo!() rather than working code" 0.98 / 0.02.

All eight correct. The rewrite that mattered most added the reason to the
question: naming *why* the edits would be separate gave the model something in
the text to check.

## Findings from reading five more recipes

- The dispatcher recipe bundles a routing choice and every branch's arguments
  into one request and reads only the branch it took. Our review asks the
  acceptance question, then asks separately which brief a reviewer needs, and
  both read the same evidence. The second call can ride along in the first.
- It also warns against computing a combined confidence as a product of its
  parts, because that falls as a decision takes more inputs whether or not
  anything is shaky. Use the minimum, and name the weakest part.
- The citation recipe separates "the evidence contradicts you" from "the
  evidence says nothing about it". Our claims-versus-output check collapses
  both, so a child that lied and a child that never ran the test look identical.
  It also does a cheap substring pass first and only pays the model for
  survivors.
- The cascade recipe derives nothing about its own threshold either, but it
  shows the method: sweep the escalation threshold against recorded outcomes and
  read the frontier. Our review already logs every judgment keyed by commit, so
  we have the material and have never used it.
- The guardrail recipe separates "act automatically" from "send to a human" as
  two independent thresholds with a severity axis that can promote one to the
  other. We have one floor and one outcome.

## Engine and workbench defects found by using it

Nine, all reproducible, in `plans/jev-lab/friction.md`. The ones that matter:

1. The workbench preamble imported `Jev.Operators (Cell ((:=)), Packet ((:&)))`
   but not `Nil`. Every Jev packet ends in `Nil`, so any packet copied from a
   skill example fails on its last line. Fixed in `tidepool/src/actor_host.rs`;
   takes effect on the next session launch.
2. A cell whose last statement is `pure x` is rejected with an ambiguous
   `Applicative f0`, and our own advice then says "give it a signature", which is
   the wrong repair. Cause: the whole-cell preflight class-dispatches on
   `Eff effects value` against a bare `value`, and a metavariable head matches
   neither. A subagent is implementing the fix now, with instructions to prefer
   making the dispatch commit over retrying the compile.
3. Declarations collide across cells while binds shadow, so you cannot iterate on
   a definition. Worse, a cell that fails partway still commits its earlier
   declarations, so retrying the same cell collides with itself. This happened
   to me twice and it is the single biggest obstacle to the workbench being a
   workbench.
4. A declaration cannot see a value bound by `<-` in the same cell. The fix is to
   write one self-contained function, which is better style, but nothing says so
   and the natural first draft fails.
5. There is no Text-to-Int anywhere in the default surface: no `T.decimal`, no
   `reads`, no `read`. Parsing `path:line:col` is routine here and I hand-rolled
   a digit fold.
6. Reading a typed answers packet via `toJSON` and key lookup hits an
   unresolvable numeric defaulting error; record dot on the typed answer works
   and is shorter. Worth stating in the Jev skill.

One positive worth copying: the mount-boundary refusal named the boundary, the
directory that would have worked, and the custody state, and I fixed my cell
correctly on the first read.

## Open questions for review

1. Is `outside_ownership` the right trigger for stopping a leaf and escalating a
   contract gap, or should a leaf be allowed to edit outside its paths when every
   such site is a mechanical consequence of a change it does own?
2. The per-location questions cost one request for nine locations. On a large
   failure that pool could be hundreds of items. Is the right bound a cap with a
   second pass, or a cheaper pre-filter in code?
3. The tree-wide search currently keys on the last segment of the symbol, which
   is cheap and worked here but will be noisy for a common name. Worth making the
   search term itself a judgment?
4. Nothing here is committed. The functions live as cells in `plans/jev-lab/`.
   Porting `look` into `.shoal/Project/` as a module that a Sol node can call
   requires a `shoal check` cycle and is the obvious next step, but it should
   probably wait until the declaration-shadowing defect is fixed, since the
   port will need iteration.
