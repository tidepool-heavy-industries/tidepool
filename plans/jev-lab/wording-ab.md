# Question wording, measured on two real failed builds

Same two fixtures, same packet shape, same model, one request each. Only the
wording changed. This is the structure-recovery cookbook's "name the narrowest
fact that decides it" lesson, reproduced on our own artifacts.

Fixture A: five `error[E0004]: non-exhaustive patterns: app::ActivePanel::Tags
not covered`, one shared `note: app::ActivePanel defined here --> src/app.rs:61:10`.
Fixture B: one clippy `field_reassign_with_default`, promoted to an error by
`-D warnings`, inside a test, whose note points one line above the primary site.

Owned paths in both runs: `src/panels/`.

## Vague wording

| question | A |
|---|---|
| Should the repair edit this group's shared location, rather than each of the listed sites? | 0.42 |
| Would a single edit resolve every listed site in this group? | 0.42 |
| Is the fix the compiler suggests in its own help text the right repair here? | 0.41 |
| Does repairing this require changing a type or signature that code outside `owned_paths` depends on? | 0.18 |
| Is this a lint that should be allowed where it fires rather than fixed? | 0.05 |

Three of five sit at 0.41-0.42: no signal at all. The two that worked are the
two that named a concrete fact.

## Narrow wording

| question | A | B |
|---|---|---|
| Is the code at this group's shared location already correct, so that the repair must change the listed sites instead? | 0.67 | 0.28 |
| Does each listed site need its own separate edit, because the sites are in different functions or files? | 0.72 | 0.12 |
| Is at least one listed site in a file that does not start with any prefix in `owned_paths`? | 0.96 | 0.89 |
| Does the compiler's own suggested fix insert a placeholder such as `todo!()` rather than working code? | 0.98 | 0.02 |

Every answer is correct. A needs five separate edits and must ignore the
compiler's `todo!()` suggestion; B is one edit whose suggestion is real code and
whose shared location is part of the edit. Three of the four questions separate
the two fixtures cleanly.

The rewrite that mattered most: "would a single edit resolve every site" (0.42)
became "does each site need its own separate edit, because the sites are in
different functions or files" (0.72 / 0.12). Naming *why* it would be separate
gave the model something in the text to check.

## What code decided without asking

Grouping. Five diagnostics with an identical headline and an identical
`note: ... defined here` target are one cause; that is a string comparison. The
model was never asked to group, and the group survives any reordering of the
input because the key is content, not position.
