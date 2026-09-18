# Lab6 notes — CALIBRATION

Live in Shoal session `lab6`, launched against a dedicated clone at
`/home/inanna/.claude/jobs/4940a626/tmp/ws6` (the shared toy-repo workspace
was locked by another lab's session; binding roots are single-owner per
workspace path — `tidepool-worktree::binding.rs`'s `BindingTable::open`).
2026-09-17. Every number below came from a real `J.ask`/`J.ask1` call; none
invented.

## 40. Speculative fan-out, measured

1. **Documented**: TypeSafe's benchmark claims 13 independent questions asked
   concurrently in one packet are ~10.0x faster (and 12.2x cheaper) than the
   same 13 asked sequentially.
2. **Ours**: reproducing the latency half against a real Rust file instead of
   a synthetic benchmark corpus, with our own `sh`/`date` timing harness
   (digit-folding parse, per brief rule 6) rather than TypeSafe's.
3. **Fixture**: `git show 53ad43c:src/store.rs` (full file, 3309 bytes,
   bound with `T.take 3200` — effectively the whole small file).
4. **Questions** (13, verbatim, all literal and independently checkable
   against `content`):
   1. Does `content` contain the exact substring `pub fn`?
   2. Does `content` import or use the `serde_json` crate?
   3. Does `content` contain a module annotated `#[cfg(test)]`?
   4. Does `content` handle a missing file by returning a typed error rather
      than panicking?
   5. Does `content` define an `enum`?
   6. Does `content` implement the `Display` trait for any type?
   7. Does `content` use `async` or `await` anywhere?
   8. Does `content` call `.unwrap()` anywhere?
   9. Does `content` read a file's contents from disk?
   10. Does `content` write a file's contents to disk?
   11. Does `content` define a `struct`?
   12. Does `content` use a `HashMap` or `BTreeMap` type?
   13. Does `content` take a parameter named `limit` that bounds how many
       items it returns?
5. **Numbers**:

   | run | wall time (ms) |
   |---|---|
   | one packet, 13 questions | 317 |
   | 13 sequential `ask1` calls | 2213 |
   | ratio (packet / sequential) | 0.143 → packet is **6.98x** faster |

   Per-question masses agreed between the packet run and the sequential run
   (both runs, in question order): `0.99, 0.98, 0.99, 0.94/0.93, 0.99, 0.98,
   0.02/0.01, 0.06, 0.97, 0.95/0.94, 0.05, 0.02, 0.97`. The two false answers
   (q7 async/await, q11 struct, q12 HashMap/BTreeMap) and the mixed answer
   (q8 `.unwrap()`, correctly low since the file uses `.expect(...)` not
   `.unwrap()`) landed the same way in both runs — the fan-out did not
   degrade individual answers.
6. **Outcome**: **worked**. 6.98x is in the neighborhood of TypeSafe's 10.0x
   but noticeably lower; on a substrate this small (13 cheap literal nouls
   over one already-short file) fixed per-call overhead is a bigger fraction
   of sequential time than it would be for costlier questions, so the two
   numbers are not expected to match exactly.
7. This absorbs the "ask N independent yes/no questions about one artifact"
   stretch of a model's own back-and-forth triage — the 7x we measured is
   real wall-clock the model would otherwise spend waiting on 13 round trips.

## 41. Position bias

1. **Documented**: TypeSafe's calibration work on order/position sensitivity
   in multi-alternative choices.
2. **Ours**: a Latin-square rotation — the same 5 alternatives asked 5 times
   in **one packet**, each rotation moving every alternative through every
   ordinal position exactly once — over a real compiler-failure transcript.
3. **Fixture**: `git show`-free — `cat` of
   `/home/inanna/.claude/jobs/4940a626/tmp/lab5/fx/53ad43c-check.out`, the
   last ~3000 characters (the actual `rustc` error block, not the Nix/Cargo
   preamble).
4. **Questions**: `"Which statement describes the failure shown in `text`?"`,
   asked identically 5 times with alternative order rotated. Alternative
   text (kept the same grammatical shape and comparable length across all
   five, per the coordinator's finding that bare labels get scored on the
   label, not the content):
   - `wrong_arity`: "`text` shows a function call site given a different
     number of arguments than the function's definition takes."
   - `nonexhaustive_match`: "`text` shows a `match` or case expression that
     fails to cover every variant of the type it matches on."
   - `lint_as_error`: "`text` shows a compiler warning or lint that has been
     promoted to a hard, build-failing error."
   - `assertion_failed`: "`text` shows a test whose `assert` or
     `assert_eq!` check failed when the test was run."
   - `none_of_these` (exit): "`text` does not name a wrong-argument-count
     error, a non-exhaustive match, a promoted lint, or a failed test
     assertion."
5. **Numbers**:

   | alternative | round1 | round2 | round3 | round4 | round5 | spread |
   |---|---|---|---|---|---|---|
   | wrong_arity | 1.0 | 1.0 | 1.0 | 1.0 | 1.0 | 0.0 |
   | nonexhaustive_match | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
   | lint_as_error | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
   | assertion_failed | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
   | none_of_these | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |

   All 5 rounds selected `wrong_arity` at mass 1.0, regardless of position.
   Every spread is 0.0, well under the 0.10 threshold that would flag
   position bias.
6. **Outcome**: **interesting failure** — not a failure of the mechanism, a
   failure of this fixture to exercise it. The compile error is so
   unambiguous (four identical `E0061` blocks, `rustc`'s own diagnostic text
   already matching one alternative almost verbatim) that the answer
   saturates at 1.0 no matter where it sits in the list, so there is no
   headroom left for position to move anything. This says nothing about
   position bias one way or the other; it needs a genuinely close call (an
   answer that would otherwise land in the 0.4–0.7 band) to be a real test.
   Per the stop rules, recording as observed rather than re-tuning to force
   ambiguity.
7. This absorbs the "does the order I list candidates in bias a dispatcher's
   choice" question a router or triage cell would otherwise have to guess at
   — worth re-running against a genuinely ambiguous fixture before trusting
   any dispatcher's alternative ordering.

## 42. Pool size and anchor drift

1. **Documented**: TypeSafe's guidance that a pool has a practical size
   before answers get noisier.
2. **Ours**: two anchors with certain ground truth, run inside pools of 5,
   15, and 30 real Rust source lines drawn from both files in the toy repo
   (padding pool nested: pool-5 ⊂ pool-15 ⊂ pool-30, same 27 padding lines
   in the same order, so the only thing that changes between runs is how
   many padding items surround the anchors).
3. **Fixture**: `git show 53ad43c:src/store.rs` and `src/main.rs` — one
   line each, real, copied verbatim: anchor-test = `assert_eq!(items,
   loaded);` (store.rs:83, inside `mod tests`), anchor-nontest = `pub fn
   save(items: &[Item], path: impl AsRef<Path>) -> Result<(), StoreError>
   {` (store.rs:50, well outside `mod tests`, which spans lines 65–113).
4. **Questions**: one noul per pool item, same wording at every pool size:
   "Is this line of Rust source code located inside a `mod tests` block (a
   unit test module), rather than in the surrounding non-test code?"
5. **Numbers**:

   | pool size | anchor_test `.yes` | anchor_nontest `.yes` |
   |---|---|---|
   | 5 | 0.82 | 0.04 |
   | 15 | 0.79 | 0.04 |
   | 30 | 0.81 | 0.04 |

   anchor_test spread = 0.82 − 0.79 = **0.03**. anchor_nontest spread =
   **0.00**. Both well under the 0.10 drift threshold.
6. **Outcome**: **worked** — no meaningful pool-size drift observed in the
   5→30 range for this membership-noul shape. Two things worth flagging
   rather than smoothing over: anchor_nontest is correctly confident (0.04,
   i.e. ~96% "not a test line") but anchor_test never exceeds 0.82 despite
   being an unambiguous `mod tests` line — the noul is systematically less
   confident on the positive case than the negative one at every pool size,
   a calibration offset, not a drift. Also, contrary to the coordinator's
   "removing alternatives makes judgment worse" finding, growing the pool
   here did not sharpen the anchors either — masses were flat within noise,
   which is a different shape of question (per-item noul, no alternatives
   to prune) than the choice-pruning case that finding described.
7. This absorbs the "how big can a triage pool get before I should shard it"
   question a batch-relevance or batch-classification cell would otherwise
   answer by guessing — 30 real short items showed no drift, so 30 is a
   safe pool size for this shape of question, at least for line-length
   items.

## 43. Description length

1. **Documented**: TypeSafe's guidance that richer, longer item/alternative
   descriptions can change judgment quality.
2. **Ours**: the same real fixture and choice from experiment 41 run twice
   in one packet at two description lengths (one-line vs. three-line, same
   grammatical shape and comparable length within each version, per the
   coordinator's finding that mismatched vocabulary or length inside one
   choice breaks it); plus a second, independent comparison — a two-item
   pool (`store.rs`, `main.rs`) asked the same noul at a 200-character
   preview vs. a 1200-character preview.
3. **Fixture**: the same `53ad43c-check.out` failure transcript as
   experiment 41 (fetched via `tail -n 70` this time, to keep the proxy
   transcript's echoed command output small); `git show
   53ad43c:src/store.rs` and `src/main.rs`, piped through `head -c 1200` to
   bound what the harness echoes.
4. **Questions**:
   - Choice (short and long versions asked in the same packet): "Which
     statement describes the failure shown in `text`?" Short alternatives
     are the exact one-liners from experiment 41; long alternatives add two
     more sentences each — a "this happens when..." mechanism sentence and
     a "the compiler/harness reports it as..." sentence — same shape across
     all five, short and long.
   - Pool noul (asked once per preview length): "Does this text document or
     implement handling for a missing or nonexistent file?"
5. **Numbers**:

   | choice version | winning key | mass | full distribution |
   |---|---|---|---|
   | one-line | wrong_arity | 1.0 | wrong_arity 1.0, all others 0.0 |
   | three-line | wrong_arity | 1.0 | wrong_arity 1.0, all others 0.0 |

   | pool item | 200-char preview `.yes` | 1200-char preview `.yes` |
   |---|---|---|
   | store.rs | 0.58 | 0.90 |
   | main.rs | 0.11 | 0.36 |

6. **Outcome**: **worked**, with a split result worth keeping separate. The
   choice comparison is void of signal for the same reason as experiment 41
   — the failure is unambiguous enough that both the one-line and
   three-line versions saturate at mass 1.0, so length had no headroom to
   help or hurt here. The pool comparison is a clean, real effect: longer
   previews raised the "yes" mass substantially at both items (+0.32 for
   store.rs, +0.25 for main.rs), moving in the same direction both times.
   The store.rs 200-char preview cuts off inside the file's doc-comment,
   just *before* the sentence "Edge cases: a missing file... must all
   return `StoreError`, never panic" (which starts at byte ~232 of a
   200-byte cutoff) — the 1200-char preview includes that sentence outright,
   which plausibly explains the jump from 0.58 to 0.90. Longer previews
   helped here because the deciding fact was textually further into the
   file than a short preview reached.
7. This absorbs the "how much of a file do I actually need to preview to
   get a trustworthy per-item judgment" question a triage cell answers by
   guessing at a `T.take` number — here, a preview short enough to be cheap
   was measurably worse than a preview long enough to include the deciding
   sentence, on the same two real files.

## Cross-experiment note

Two of these four cells (41, and the choice half of 43) turned out to probe
a fixture with no real ambiguity, so they returned confident, uninformative
1.0/0.0 splits rather than anything about position or length. That is not
nothing: it says position and short-vs-long description wording do not
rescue (or hurt) a judgment that is already this easy, and any future
position-bias or description-length probe should deliberately pick a
question sitting in the 0.4–0.7 range *before* varying the thing under test,
or risk the same null result.
