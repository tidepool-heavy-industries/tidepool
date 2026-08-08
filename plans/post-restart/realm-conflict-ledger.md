# Conflict ledger — realm-spike lane

This lane was launched deliberately overlapping the extract wave's runtime
territory to measure what merge conflicts actually cost (Inanna, 2026-08-08:
"a test to see if we're being too timid about merge conflicts"). The ledger is
a first-class deliverable — the experiment's data, not bookkeeping.

## Recording rule

One row per rebase/fold conflict, however boring. A fold that produced ZERO
conflicts is also recorded (as a zero-row entry with the branch and the files
it touched) — "no conflict" is the measurement, not the absence of one.

Stop-and-ask applies only when a resolution would DROP a side's behavior.
Mechanical resolutions are logged and moved past.

| # | when | operation | file | hunk shape | resolution | minutes | side dropped? |
|---|------|-----------|------|------------|------------|---------|---------------|

## Zero-conflict folds

| # | when | operation | branch | files touched | note |
|---|------|-----------|--------|---------------|------|
| Z1 | 2026-08-08 | `merge` into `root.realm-spike` | `root.realm-spike.lifetime` | 4 files: `plans/post-restart/spike-notes/realm-lifetime.md`, `tidepool-codegen/tests/realm_root_growth.rs`, `tidepool-codegen/tests/realm_module_growth.rs`, `tidepool-codegen/src/jit_machine.rs` | Clean. Note the fourth file: this lane DID edit `jit_machine.rs` — the file the extract wave's runtime territory also touches — adding two accessors (`functions_defined`, `old_space_bytes_used`) into the existing accessor block near `persistent_roots_count`. Additive `impl`-block insertion, no conflict against the fork point. |

## Running tally

- Conflicting folds: 0
- Zero-conflict folds: 1
- Total resolution minutes: 0
- Sides dropped: 0
