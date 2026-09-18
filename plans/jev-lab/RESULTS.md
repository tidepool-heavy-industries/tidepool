# What the three failure shapes taught the investigation

`Project.Investigate` was built against one failure, then run against two more.
Each new shape found a real defect. This is the record of what changed and why,
because the defects are more useful than the successes.

The three shapes, all real failed checks of the toy repo:

| commit | shape | why it is different |
|---|---|---|
| `f726882` | five `error[E0004]` non-exhaustive patterns | many diagnostics, one shared cause, a symbol named in the headline |
| `4610b5e` | one clippy lint promoted by `-D warnings` | one diagnostic, no symbol, the note is part of the fix, and it is inside a test |
| `53ad43c` | four `error[E0061]` wrong arity | no symbol, the cause is a definition the caller did not change, and the fix could go either at the definition or at every call site |

## What the second shape found

The headline of a lint carries no backticked symbol, so the tree was never
searched and the report said so in its coverage. Honest, but useless.

Fix: when no symbol is recoverable, fall back to the definition enclosing the
primary site, found mechanically from its `fn` header. On `4610b5e` that
recovered `tags_panel_is_a_focusable_shared_panel`, which is the right anchor.

It also exposed a rendering defect: with no symbol, the report read "repairing
error: field assignment outside of initializer for an instance created with
Default::default() requires edits", inlining a whole headline into a sentence.
The subject is now the symbol, or the search term, or the words "this failure".

## What the third shape found, and it is the important one

Three defects, in increasing order of how much they mattered.

**The search anchored on the wrong end.** For a wrong-arity error the primary
site is the *call*, so anchoring on its enclosing definition searched the tree
for `run`, the function that happened to contain the first call. Two noise
locations, nothing useful. The anchor is now the location a `note: ... defined
here` points at, tried before the primary site, because that names the thing
that actually changed. The same fixture then searched for `load` and found ten
candidates instead of seven, including a caller the compiler had not reported.

**Locations the floors could not route vanished from the report.** Nothing said
so. A reader saw a short list and had no way to know that locations had been
examined and dropped. The report now counts them: "3 examined locations fell
below every floor and are not listed above."

**The judgments sat on the floor, and the floor made the answer unstable.**
This is the real finding. Raw per-location answers on `53ad43c`:

| location | must change |
|---|---|
| `src/store.rs:59` the definition | 0.66 |
| `src/main.rs:83` the caller | 0.61 |
| `src/store.rs:81` a test caller | 0.59 |
| `src/store.rs:100` a test caller | 0.42 |
| `src/store.rs:110` a test caller | 0.37 |

Two runs of the same input put `src/main.rs:83` on opposite sides of a 0.6
floor. That is not the model being unreliable; the question genuinely has two
answers. A wrong-arity error can be repaired by reverting the signature or by
updating every call site, and nothing in the diagnostics decides which. A hard
floor converts that into a confident list that changes between runs.

Fix: an explicit undecided band. Between `mustChangeUnclear` (0.35) and
`mustChangeFloor` (0.6) a location is reported as undecided, with its
probability, under a heading that says the report will not decide. On this
fixture five locations land there, which is the correct shape of the answer.

## The current output on the third shape

```
bash ./check.sh exited 101 with no symbol named; searched for the enclosing
definition load, because the diagnostic names no symbol

must be edited:
  - src/store.rs:59  (note: function defined here)

leave alone:
  - src/store.rs:18  (found by searching the tree for load)

undecided, and the report will not decide for you:
  - src/main.rs:83  (reported by the compiler in G1; 51 in 100 that it must change)
  - src/store.rs:81  (reported by the compiler in G1; 59 in 100 that it must change)
  - src/store.rs:100  (reported by the compiler in G1; 36 in 100 that it must change)
  - src/store.rs:110  (reported by the compiler in G1; 38 in 100 that it must change)
  - src/store.rs:83  (found by searching the tree for load; 48 in 100 that it must change)

related test locations:
  - src/store.rs:81  (exercises the code but asserts no listed requirement)
  ...

coverage:
  - 10 of 10 candidate locations examined
  - 4 test bodies read
  - 3 examined locations fell below every floor and are not listed above
  - searched for the enclosing definition load, because the diagnostic names no symbol
```

## What the consumer said

A Sol agent was given the third failure as a real task, owning `src/store.rs`
and nothing else, and told the module existed. It used it, and its verdict was
blunt:

> the probability routing contradicted explicit compiler errors. It treated
> obvious broken call sites as uncertain and included noisy search matches. I
> had to verify everything against the raw output and git grep.

> I did not trust the "undecided" classification. [...] The section did not
> improve my answer.

> I would miss only convenience: automatic grouping of diagnostics, a quick
> list of candidate locations, a reminder to consider ownership and search
> coverage. I would miss no essential diagnostic insight. Raw compiler output,
> git show, and git grep were more reliable here.

It also reached the opposite conclusion about the repair: the call sites are
the obligations and the definition at line 59 is the intended API change. And
it noticed the instability directly, saying that if another run had put line 59
under "must be edited", that run was wrong.

## The general lesson

Every one of these defects was a place where the program turned an uncertain
judgment into a confident statement, or dropped something without saying so.
Grouping and the ownership check were mechanical and correct from the start.
The search term was not: its anchor was wrong until the third fixture, and
that was a mechanical fix rather than a question of doubt.

The deeper fault is the one the consumer named. Asking "must this location be
edited?" of each location independently is incoherent when the failure admits
two whole repairs. Under "keep the new signature" every reported call site is
an obligation, mechanically, with nothing to judge. Under "restore the
signature" none of them is. Asking without fixing the strategy first produces
answers near a half, and a reader who knows the domain correctly distrusts
them. The repair strategy is the question, and the location questions are
conditional on its answer. Nothing in the diagnostics settles the strategy,
because the strategy is a question about what the author intended.
