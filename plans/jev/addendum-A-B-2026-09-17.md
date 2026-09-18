# Addendum: reflex table (A) and gate control rerun (B), 2026-09-17

## B: gate control with the verbatim run 3 state, 8 repeats each

Policy is the run's own: minMass 0.55, minMargin 0.20, minConfidence 0.50.

| Options | Winner | Mass | Margin | Conf | Distribution |
|---|---|---|---|---|---|
| a verbatim (both options argue) | sufficient 8/8 | 0.90 ± 0.01 | 0.80 | 0.85 ± 0.01 | sufficient 0.90, need_more 0.10 |
| b action-descriptive ("Launch the coding child now") | sufficient 8/8 | 0.72 ± 0.02 | 0.45 | 0.58 ± 0.03 | sufficient 0.72, need_more 0.27 |
| c condition-descriptive ("The report names a target file, a duplication check, a scope, an acceptance criterion, and its failure evidence") | sufficient 8/8 | 0.95 ± 0.00 | 0.91 | 0.92 ± 0.00 | sufficient 0.95, need_more 0.02, defer 0.03 |

The verbatim state reproduces the run (0.90/0.85 here against 0.82/0.72 in the run,
same side of every floor), so my earlier reconstruction was the discrepancy, not Jev.

The two descriptive rewrites pull in opposite directions, and that is the finding.
Describing the action ("launch now", "send a follow-up") gives Jev nothing in the state
to match against, so the distribution flattens toward the prior. Describing the
condition under which the option applies turns the gate into a checklist over
`report_evidence`, which Jev can verify field by field, and confidence goes to 0.92 with
zero variance. Rule 2 sharpened: options describe the condition that makes them apply,
stated in terms of fields the state has. Not the action, and not the argument.

Literal option text for doc jev:

```
sufficient:     The report names a target file, a duplication check, a scope, an acceptance criterion, and its failure evidence.
need_more:      At least one of target file, duplication check, scope, acceptance criterion, or failure evidence is missing from the report.
defer_to_model: The report has all five items but they conflict with each other or with the goal.
```

Note the defer option is now a described condition too, and it picked up 0.03 instead of
0.01, which is the first time it has registered at all.

## A: the reflex table

`reflex_table.json` alongside this file. Every rustc code, lint name, GHC code and clippy
behavior marked `verified: true` was produced by compiling a planted break on rustc
1.93.0 / GHC 9.12.2 / clippy 0.1.96 today; the four marked false are from rustc's own
catalog and were not exercised. Shape:

- `next_steps`: five values. `cargo_fix`, `run_fmt`, `rerun`, `llm_patch`, `escalate`.
  `delete_line` is gone: unused imports and unused mut are machine-applicable and
  `cargo fix` handles them; deleting a dead function or an unused variable is a judgment
  and goes to `llm_patch`. `add_import` is also gone: a diagnostic code alone does not
  establish that adding an import is the repair (the case that removed it was a model
  applying a function to the wrong one of two similarly-named result types, which no
  import would fix), so every entry that selected it now goes to `llm_patch` with a note
  to inspect the source and the diagnostic before choosing an import or a definition
  repair.
- Precedence, first match wins: rustfmt `Diff in` → environment markers (locks, build
  scripts, linker, network, disk, dependency resolution) → the first error's E-code or
  lint name → rustc code-less parse errors (`error: expected ...`) → test-runner markers
  (`TIMEOUT [`, signals, `assertion ... failed`, `panicked at`, `FAIL [`) → Jev over the
  code-less class list.
- Clippy needs no allowlist. `cargo clippy --fix --allow-dirty` applies exactly the
  machine-applicable suggestions and nothing else, so the tool is the allowlist. Verified:
  needless_return, map_clone, clone_on_copy, useless_vec and len_zero were applied in one
  pass, and the pass surfaced two new lints (iter_cloned_collect, const_is_empty). So the
  reflex is fix, rebuild, repeat up to three times, then llm_patch for whatever persists.
- E0425, E0433, E0412, E0599 and GHC-76037 are all `llm_patch`, each with a note on what
  the message text still tells you: E0425/E0433/E0412 mention a `help: consider
  importing` line, E0599 an `items from traits can only be used if the trait is in scope`
  line, and GHC-76037 a qualified name versus an unqualified record field -- useful
  context for the model, but none of it is a standing license to skip inspecting the
  source and the diagnostic before choosing an import or a definition repair.
- GHC-18042 is the -Werror wrapper and carries no class; take the other code on the line.
  GHC-39999 covers no-instance, ambiguous-type and too-few-arguments (which surfaces as
  `No instance for Show (Int -> Int)`), all `llm_patch`.
- The code-less Jev class list has eleven MECE entries named by what the output literally
  shows: syntax_error, type_mismatch, name_resolution, ownership_or_lifetime,
  instance_or_trait, unused_or_style_lint, test_failure, test_hang_or_crash,
  build_environment, multiple_unrelated, other. The E5 taxonomy misses (E0004, E0063,
  E0277 → other) are gone because those all have codes now and never reach Jev.

Expected effect on the E5 number: of the 30 real outputs, 23 carry a code or a lint
name and never reach Jev; the 7 that do are the five test outputs and the two rustfmt
diffs, and rustfmt is caught by the `Diff in` marker. So Jev sees the test outputs, where
it was 5/5 on class at 0.98 to 1.00. The escalation ratio on that population is the one
worth measuring on run 5.

## Real-run latency, from the run 1 to 4 host logs

21 Jev calls logged with elapsed_ms: min 90, p50 140, p90 198, max 626 ms, all status
200. Consistent with the laptop's 213 ms median over 262 calls.
