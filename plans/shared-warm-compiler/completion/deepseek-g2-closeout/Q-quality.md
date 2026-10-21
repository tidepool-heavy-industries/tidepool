# Q leaves: bounded quality work that removes obligations

These are four independent assignments, coordinated directly by S. Each has one
primary outcome and nearby review questions. Use separate checkouts, agree shared
hunks and return small candidates. Do not turn an aesthetic pass into a framework
migration. Each parcel must improve one axis without quietly weakening another.

## Q1 — Logging contract and scoped acquisition

Primary: make the existing structured logger's guarantees match its public
surface and actual Rust transport. Adjacent: remove unused exported helpers,
protect partial acquisition, preserve asynchronous cancellation.

Read `peer-review-quality.md`, `Logging.hs`, `LoggingTest.hs`,
`ExtractUtil.hs::trySynchronous`, and the consuming daemon's stderr routing.
Existing production logs are trace events with small static field lists. There
is no demonstrated production memory/cancellation incident; don't inflate one.

Current text fields are capped, but field count, total event size and arbitrary
Integer rendering are not. Prefer a small finite diagnostic value vocabulary and
a field-count bound with a stated operational purpose. Derive/document its total
worst-case encoding bound, including JSON escaping, before claiming bounded
events. Use current maximum thirteen fields and finite Int-derived counts as
the workload basis. Avoid a complex exact-byte budget manager if simple finite
inputs suffice. If a cap would discard useful fields, make omission explicit.
Disabled trace must avoid constructing/rendering expensive observations.

Default transport direction: preserve declared severity in the existing Rust
tracing owner with a small typed record decoder and exhaustive level mapping.
Retain the compiler target/event/fields and useful malformed-line evidence;
malformed diagnostics must not change compile success or become source errors.
Keep the existing structured-trace versus pane filtering policy explicit. Do not
claim every RUST_LOG directive controls a sink that actually uses its own filter.
No async appender, mutable global logger registry or diagnostic policy engine.

Use the real emission boundary in tests. `logEventTo`, `renderLogEvent` and
`newLogger` currently have test-only external consumers; remove unneeded public
surface where the actual logger can be exercised, without creating a second
logger/test-support API. Keeping a justified compiler-internal constructor is
better than a new elaborate hiding mechanism; state its real consumer.

The test helper's `openTempFile` then `hClose` occurs before its outer bracket
owns release. Protect the partially acquired pair with an existing helper or
nested bracket/onException, retaining primary error and cleanup evidence.
Existing production hook/view-directory scopes already bracket their resources;
do not rewrite them without a concrete defect. Keep `SomeAsyncException` escape.

Acceptance: actual sink emission and suppression, declared bounds/omission,
ordinary emission failure not replacing the compiler result, an async exception
escaping the emission path, typed severity and malformed-record routing, and
partial-acquire cleanup. Preserve direct/resident launch and environment override
checks. Compile Rust and Haskell consumers; use existing test targets.

## Q2 — Census updates use census facts

Primary: remove full `ArtifactPayload` equality from `ViewLease::insert_payload`
when deciding whether owner/classification counts change. Adjacent: replacement
and retirement consistency; no coupling of census shortcuts to admission.

The census depends on old/new owner and kind, not equality of the full retained
program. Keep authentication/admission separate. Derive the minimal old/new
census key from actual payload variants, update only changed contributions, and
delete the expensive comparison if no other consumer needs it. Reuse the kind
representation introduced by the existing HOME/package fix.

Use the existing independent recomputation property over insert, identical
replacement, different payload/same classification, changed owner/kind, removal
and retirement. Include overlapping HOME/package ownership. Keep generic owner
queries and HOME-specific semantics distinct. Do not introduce another index,
cache, public testing factory or owner-name heuristic.

Report structural work removed separately from timing. A benchmark is optional
unless making a latency claim; meaningful census histories are mandatory.

## Q3 — Trace work should not rescan the graph per owner

Primary: eliminate repeated full observation/dependency scans while building
finalized-owner trace events in `GhcPipeline.hs`. Adjacent: disabled-trace
laziness, stable diagnostic meaning and D's timing protocol.

The reviewed loop around 6707–6744 scans graphs/modules/sources/prepared modules
for each owner. Prefer one request-local aggregation of immutable facts under
the enabled-trace branch, or emit already-known facts at their existing owner.
Do not retain a mutable cross-request cache. Preserve the actual meaning of each
count, including duplicates/order where the prior count intentionally includes
them; don't optimize by changing the metric unnoticed.

Coordinate D's dirty trace hunks before editing. Human debug lines can move into
the logger. Consumed timing/reuse/memo/retention/phase records retain their current
protocol owner unless their consumer migrates in the same candidate.

Acceptance: compare trace values to an independent enumeration over a bounded
real history, verify disabled tracing does not demand the aggregation, and keep
actual production diagnostics usable. Record count/work reduction, not an
unmeasured speedup. No new general profiling framework.

## Q4 — Delete Original authority reconstruction

Primary: after `402528b832` lands, remove the Rust attempt to reinterpret a
facade Original as another defining owner. Adjacent: re-export behavior, precise
typed mismatch errors, dead helper/caller deletion.

`authenticate_original_binding_membership` in certified_products.rs searches
selected modules and retained inventory when the claimed owner differs from the
binder's defining module. This compensates for the producer defect being removed.
The consumer should authenticate the exact presented Original or refuse; it
must not find a different authority for the caller.

Keep actual direct Original, HOME re-export and package re-export behavior through
the corrected producer. A malformed owner/binder pair refuses. A facade remains
valid listing provenance, but cannot lend its implementation identity to a
definition it exports. Remove the search, dead diagnostics and APIs with no
remaining production consumer. Do not add another global resolver.

Coordinate distinct certified_products.rs hunks with P/W and have P or R review
the joined chain. Tests must enter through the real defining-Name production
path; don't merely construct a now-valid enum in the test.

## Cross-parcel quality review

Use finished workers to review other parcels for stale callers, duplicate owner
logic, error strings driving branches, full-snapshot copying, obsolete tests and
unnecessary exports. Each report separates confirmed defects, useful deletion,
deliberate behavior and untested hypotheses. A broad grep with no hits is not a
proof that a replacement is the sole production path.

Stop at these bounded repairs. Larger capability rewrites, an async logger,
changes to the initialization scheduler and new test frameworks are outside B.
Preserve the existing single closed-support owner and affine initialization
guard; don't undo successful simplification while cleaning nearby code.
