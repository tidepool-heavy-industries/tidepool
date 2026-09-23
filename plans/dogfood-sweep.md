# Dogfooding improvement sweep

Implementation work in flight. Baseline: `54bb13992`; live run
`a8f054b6-86de-4cc2-86db-daf08d624e8c` used older binaries than that revision.
The run and its artwork are retained. Existing unrelated checkout changes
are excluded from this work's commits.

## Accepted decisions

- Explain latency before compiler redesign. Extend existing tracing and run-map;
  do not add a parallel journal, registry, scheduler, or cache.
- Preserve exact-incarnation authority, snapshot inheritance, coherent source
  capture, submitted-candidate integration, and no-replay input recovery.
- Broad source API cleanup is allowed. Static Label values become compile-checked
  `[label|...|]` literals; dynamic values use total `labelFromText` validation.
- This wave cleans observed friction and correctness defects. Field notes,
  routing advice, workspace profiles and worker retrospectives are separate
  feature work; existing unfinished modules are preserved untouched.
- One integration owner coordinates schema generation, submodule pins and tests.
  No parallel broad builds; focused owner checks first, one final full gate.

## Implementation checklist

- [ ] Baseline/provenance and correlated phase metrics: actor wait, Git capture,
      extractor queue/service, installation, effects, bounded presentation,
      notification delivery; no added secret or payload logging.
- [ ] Extend bounded run-map with provenance, timing distributions, failures,
      delivery/cleanup evidence, tree/transcript references and explicit gaps.
- [ ] A1: actionable foreign-handle diagnostics; fresh-run validation of the
      already-built inherited-turn boundary fix; own request remains usable.
- [ ] A2: existing Git owner waits up to 30 seconds for coherent capture, logs
      waiting/holding phases; cancellation and partial admission remain safe.
- [ ] A3/A12: drop obsolete queued watch prompts at presentation, remove stale
      poll instructions, expose/coalesce uncertain delivery without replay.
- [ ] A4: input acknowledgement independent of presenter; bounded native output
      and reflection before Haskell materialization; explicit paging/omission.
- [ ] A5/A6: repair checkpoint/fallback guidance and distinguish hosted lookup
      queries from executable Haskell.
- [ ] A7/A8/A13: checked labels and compiled coordination examples; clear rejected
      binding semantics and exact submitted-candidate integration.
- [ ] A14: intentional embedded Codex mode is informational; unexpected fallback
      still warns.
- [ ] A10/A11: report installed hooks, trace and transcript availability honestly
      through existing status/run-map; document missing capabilities as untested.
- [ ] Matched cold/warm and concurrent workload measurements; attribute costs
      before proposing batching, display reuse or immutable spec reuse.
- [ ] Authoritative generation, compiled snippets/fixtures, both workspace checks,
      formatting and diff checks, fresh nested Luna run, final verify and review.

## Work ownership

- Lead: shared integration, provenance/phase instrumentation, capture waiting,
  API/schema coordination, generated outputs, verification and final run.
- `audit_root_interview`: queued watch delivery and pending-inbox diagnostics.
- `codex_child_stall`: command input acknowledgement and bounded observation.
- `sweep_observability` (Sol 6): run-map summaries and its CLI presentation.

## Baseline evidence

Audit reports: `/tmp/exomonad-a8-issue-catalog.md`,
`/tmp/exomonad-a8-root-interview.md`, `/tmp/exomonad-a8-child-interviews.md`,
`/tmp/exomonad-a8-trace-audit.md`.
Baseline trace ends at `2026-09-23T11:10:41.076Z`; later interviews are excluded.
95 Haskell calls had median elapsed 14.91s, lower-rank p95 73.63s. These are
overlapping outer spans, not compiler time. 457 extractor requests had median
662ms and p95 11.5s; the old trace does not separate queue from compiler service.
44 Jev RPCs had median 167ms. Counts differ by observation level; do not derive
failure percentages from mixed units.
