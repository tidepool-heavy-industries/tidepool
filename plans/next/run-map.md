# Run-map TL — artifact-first visibility

## Assignment / boundaries

Build a reproducible, bounded run map from existing Shoal artifacts. User explicitly
wants inspection of the actual overnight run, not reconstructed narratives from
source. No Tidepool dashboard: interactive UI belongs in shoal-repl later. Do not
add another durable log, registry or provider tracer. This branch can proceed in
parallel with service/control work; request missing instrumentation from its owner
instead of editing launch/control files concurrently.

Baseline before handoff: `e5a1842dd4d3302d6eb8c0519a678937599d9c7e`.
Start with `overnight-evidence.md` beside this document for concrete paths, observed
counts and limits; it is evidence data, not required conversation context. Read root
and nearest AGENTS. Existing owners: actor lifecycle/mailbox in `tidepool-actor`,
run composition in `tidepool/src/shoal.rs` and `actor_host.rs`, provider usage in
`tidepool-agent/src/backend/codex/rollout_usage.rs`, JSONL/version handling in
`tidepool-repr`. Inspect production consumers before selecting report entry point.

## Product contract

Given an explicit run directory and optional time window, produce machine-readable
and concise human-readable derived views linking:
- Actor/incarnation, parent/admission and source seed, provider thread and launch.
- Assignment/amendment/wake/reply where actually recorded; failed launch remains a
  node even with no provider binding. Actor count is not peak concurrency.
- Candidate, review, integration, baseline delivery/incorporation and tested revision
  when evidence supports them. Do not infer acceptance from a commit or reply alone.
- Launch/admission/presentation failures, resource lifecycle and missing artifacts.
- Per-response usage, cache counts and explicit accounting window/provenance.

Represent Observed / Inferred / Unknown distinctions in actual types. Unknown edges
must remain unknown; labels and narrative summaries are not authoritative IDs.
Derive from existing records without modifying raw evidence. Make useful partial
reports on truncated JSONL, missing bindings, rotated tmux history and stale paths.
Keep full-context provider captures private and bounded; do not commit raw prompts,
credentials or complete rollouts into test fixtures. Sanitize small fixtures.

Usage: filter records to bound thread IDs; dedupe response IDs with conflict detection;
sum per-response usage, **not cumulative token_count totals inherited through forks**.
Apply explicit time cutoff to exclude later retrospectives. Report missing coverage,
reasoning as output subset, no price claim without pricing/model evidence. Cache hit
ratio does not establish normalized prefix equality, latency or causal savings.

## Work tree / review

Scaffold a small reader/report contract and representative sanitized fixture. Fork
artifact-reader implementation and independent historical reconciliation/tests on
disjoint files. Reuse existing CLI/report owner; avoid a parallel general telemetry
framework. Reconcile observed numeric campaign map/usage with the evidence index;
report discrepancies rather than force totals. Fresh reviewer checks malformed/
partial input and double counting, then requests repairs directly.

Deliver candidate, invocation/example output, artifact provenance, exact focused
checks and limitations. If a needed event was never recorded, return a minimal
instrumentation request to owning lead/root; do not invent it. This branch must
remain useful before the new service topology lands.
