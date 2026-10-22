# Q: bounded quality repairs; keep accepted simplifications

> Historical assignment, superseded at the 5c759554 review. Use [NEXT-WAVE.md](NEXT-WAVE.md) and STATE for current work. Preserve useful contracts below; do not launch this old task list unchanged.

Primary outcome: use the existing typed JSON ecosystem for logging and give
the historical fixture an honest ownership contract.
Adjacent questions: cancellation/error fidelity, diagnostic-only overhead,
and tests that mirror incidental implementation.

Q2 census, Q3 aggregation and Q4 fallback deletion are already integrated and
source-reviewed. Do not restart those parcels. Their evidence limits remain in
REVIEW-c910d1 and tests-quality-review.md.

## Q1 follow-up: delete the handwritten scanner

In extract-cmd daemon.rs, structured_log_level/json_string_at returns after
recognizing level without validating the rest of JSON. An unterminated object
containing a valid error level is classified Error rather than malformed.
Escaped valid keys/values also behave inconsistently.

Replace it with complete serde decoding of a small typed record/closed level.
Validate the schema/required shape the producer actually emits. Preserve the
original line. Any decode/shape failure takes the existing debug/malformed
diagnostic arm; it must never alter compiler outcome.
Use the existing dependency family, declare the production dependency if
needed, and regenerate owning Buck inputs. Delete the scanner and its callers,
rather than patching individual syntax cases.

Test through log_compile_timing and the real subscriber: normal levels,
malformed-after-valid-level, truncated object, valid JSON escaping,
unsupported schema, duplicate level and unknown level according to the
explicit contract. Define duplicate handling rather than silently choosing
first/last. Keep timing/END/reuse protocol parsing unchanged.

Preserve Haskell's 32-field/event byte bound, disabled-trace early exit,
SomeAsyncException propagation, synchronous sink-failure behavior and corrected
partial-acquisition bracket. Do not turn cancellation into ordinary logging
failure. The default pane threshold is warn; document/test whether Error/Warn
events should surface there without inventing a new sink.

## Frozen fixture ownership

The committed historical CBOR/HI corpus is genuine authenticated transport,
not a current compiler issuer. Its test directly restores/individually issues
graphs and does not qualify the new occurrence reader. Coordinate P3's rename
and replacement current-compiler controls.

When P2 deletes resolve_held_package_binding, remove the frozen test's calls to
that obsolete selector too. Retarget it to actual versioned byte/product restore,
decoded identity and hash/provenance refusals. Do not retain an implementation
solely for its ambiguity test. Call it graph recovery only if it actually
materializes and reopens the issued graph.

Keep this exact counterexample until a replacement is accepted. Approved
bounded direction: retain it ONLY as a frozen reader/recovery regression, with
a narrow documented exception to generated-artifact policy at the owning
fixture boundary. Do not broaden that exception to current prepared corpora.

Add committed provenance and a typed, validated manifest: capture source/
command where established, otherwise the exact durable capture receipt and
explicit unknowns; content hashes; accepted wire/ABI/compiler profile; test
purpose; migration/removal rule. Validate schema/kind/identity fields currently
ignored by the loader. Missing provenance is not permission to invent it.
Old bytes are never restamped with the current compiler to make tests pass.

Current compiler/selection qualification must enter declared fixture producers
and the production receipt reader. A frozen reader regression cannot substitute
for a current multi-target or World-item history. If compatibility is intentionally
broken, preserve evidence and explicitly retire/migrate that reader test;
do not silently regenerate its expected digests.

## Small optional follow-ups, after required repairs

Q3 retains full lists where only the first two path/observation labels are
displayed. A bounded prefix plus actual total/set facts can reduce retention
without changing meaning. Only do this if exact order/duplicate semantics are
maintained and a small independent enumeration control proves them.
No measured latency benefit is claimed.

Q2's repeated census test oracle can share one independent recomputation helper;
do not expose new test authority APIs. Q4's remaining exact-payload helper
comment should describe its narrower custody check, not the deleted facade
authority reconstruction.

These are optional edits, not reasons to postpone a frozen release. Report
any incidental category findings separately with source and counterexample.
Compile affected production/test targets, run precise routing/logging controls,
format and inspect deleted surface. R-N independently reviews the candidate.
