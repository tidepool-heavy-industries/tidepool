# Run-4 companion findings (2026-08-20)

The harnessed model's own prioritized improvement list from the play run's
turn-3 synthesis (steered: self-referential UX + deep recursive forking),
distilled and mapped to owned work. Source of truth for the quotes: the run-4
archive (`~/.cache/tidepool-dogfood/tidepool/selfharness/run4-archive/`,
transcript.jsonl, last FoldDecision). Written from inside the tree,
evidence-graded — treat as a first-class design input per the optimization
loop (CLAUDE.md: model usage drives UX changes).

## Its list, mapped

- **P0 — contract witness in the brief** ("compiler-derived": the ACTIVE row,
  pinned finalizer, fully-qualified resolved verb names + inferred
  signatures + adapter path, derived from the compile target, not
  driver-asserted; resolution/row/adapter/finalizer failures reported as
  distinct classes). Status: row line now derives from the real target
  (delegate-row-truth); the fuller witness (resolved signatures per verb) is
  NEW design work — natural companion to the row-poly sweep's acceptance.
- **P0 — advertised verbs must typecheck, with a verified runnable template
  each.** Status: fork/delegate fixed (row-truth-completion);
  note/getStateJson/askUserWith blocked on the QUEUED row-polymorphism sweep
  (operator decision: Member constraints everywhere). The sweep's acceptance
  should BE the verified-template requirement: one compiling probe per
  advertised verb, in the brief or pinned by test.
- **P1 — classification + commit legibility.** Status: largely shipped this
  wave (per-block failure reports name what persisted; GHCi statement
  splitting). Remaining: teach the three bind idioms (`let` / top-level decl /
  `x <- action`) and their persistence semantics in one framing sentence.
- **P1 — machine-readable tree economics.** The deep-forking answer: depth-1
  is the path of least resistance because a window cannot SEE the economics —
  it wants per-node REMAINING depth/slots/rounds, child statuses with concise
  typed summaries at the parent, artifact counts, voluntary-vs-depth-forced
  finish flags, whether a child can run fresh probes vs only synthesize, and
  a required split rationale naming intended subquestions. Status: NEW —
  the strongest candidate for the next companion iteration; overlaps the
  queued depth-cap-legibility item (forced-finish reasons are one field of
  this contract).
- **P2 — askUser schema previews** (show the JSON shape + Generic/FromJSON
  requirements before first use; label getStateJson as start-of-window
  snapshot). Status: partially moot once askUserWith compiles; keep the
  snapshot-semantics label.
- **P3 — lifecycle-aware name diagnostics** (undefined vs shadowed vs expired
  vs failed-to-commit vs durable). Status: backlog; the decl-plane already
  has the data.

## Also validated by this run

- Steering visibly redirects exploration (operator-r4's verdict, turns 2-3).
- Durable declaration vocabulary + non-destructive later failure works and
  the model USED it (Substrate/appendSample/score checkpoint chain).
- Cache-affinity baseline: 45% cached input tokens with alternating
  round-to-round misses (fixed post-run: oauth session-id; next run is the
  A/B).
