# Tidepool Katas — 2026-07-02 sweep

Ten hard, general problems (deliberately NOT mapped 1:1 to effects; the regen
criterion for 6-10: realistic but hard-or-near-impossible in bash). Every kata
executed live in one tidepool-repl session; every bug found was fixed same-day.

| # | Kata | Result | Bugs shaken out |
|---|------|--------|-----------------|
| 1 | Mini-language interpreter (parser combinators + AST + eval, from scratch) | Full Functor/Applicative/Monad stack on a newtype'd function; precedence/associativity/unary correct | 2 user bugs (backtick-`orElse` infixl-9 precedence; dependent-decl staleness) |
| 2 | Import-graph cycle audit (fixed-point closure) | 41 modules, ZERO cycles | — |
| 3 | Identifier typo hunt (bucketed Levenshtein DP) | 1611 fn names, 30 near-pairs, all legit siblings | — |
| 4 | Markdown link checker (FilePath algebra) | 1 broken link repo-wide: README.md → LICENSE (missing file) | `normalise` absent from vendored FilePath |
| 5 | Debt ledger with blame ages (grep × git blame × fmt) | 4 markers repo-wide, oldest 20d | **classifier lacked QuasiQuotes; [fmt\|] quoter capture (mkName → 'quotes); BUG-8 (Double-backed Number, silent int corruption >2^53) — all fixed** |
| 6 | Dead-parameter analysis (ast-grep metavars × word-boundary analysis) | 0 dead params in 3 crates | — |
| 7′ | Heap-resident inverted index (8163 terms, ranked multi-word search) | Instant re-query across turns — "heap as database" | — |
| 8 | API-complexity ranking (depth-aware signature parser) | 611 sigs; top zipWith4=17, hyloM=15; 13 complex-undocumented | — |
| 9 | Discrete-event simulation (4-slot semaphore, real burst trace 60×) | MAX_CONCURRENT_EVALS=4 blocks 2% (p99 4s); 2 slots → 19%; 8 = headroom | — |
| 10 | Rename-impact planner (LSP refs × doc grep × classification) | remap_generated_coords: 11 code refs + 1 doc, per-site classified | — |

Also fixed during the sweep (infrastructure the katas tripped over):
- repl effects-dir self-heal per turn (rm -rf ~/.cache/tidepool mid-session
  no longer bricks the server until restart)
- diagnostic dedupe keys by CONTAINMENT (show-se subset copies collapse even
  when the logger copy carries Suggested-fix/gutter lines)

Standing capability notes:
- Fluent one-item programs (fold-reads + join + analysis) are the grain.
- The heap is the differentiator: kata 7′'s index answers any number of later
  query shapes without re-scanning; kata 1's parser stays callable all session.
- `P.!!` is the deliberate escape hatch for indexing (Prelude omits partial
  `!!`; hit twice in the sweep — remember the P. prefix).
