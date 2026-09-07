# Bounded native usage contract

Owner authorization: retained Request28 from service actor1@1 permits disjoint
rollout_usage.rs and adjacent tests. Baseline 3bc5f8be; no control, manifest or
shared export edits authorized here. This is offline artifact work.

The existing usage owner gains `read_bounded_usage<R: BufRead>` over an explicit
iterator of `(source_label, io::Result<R>)`, a nonempty bound-thread selection,
UTC millisecond [from, until) window, and source/line/line-byte/response limits.
No implicit provider-home discovery. Missing files remain typed diagnostics.

The function returns source coverage, sanitized per-response records with
source/line provenance, conflict diagnostics and a checked aggregate of observed
nonconflicting responses. Every result is a selected recorded subset, never
billing/history completeness. No response evidence yields `None`, not zero;
a recorded zero-token response can legitimately aggregate to zero. Input/cache,
output and reasoning (output subset) remain distinct. Cumulative token_count is
never a fallback. The sole existing `record`/`parse_usage` parser supplies usage.

Response IDs deduplicate across explicitly supplied inputs. Changed thread,
turn, normalized timestamp or usage under the same response ID is a conflict;
exclude that ID from totals rather than silently keeping a winner. Bounded
records/diagnostics retain no raw JSON, message bodies or provider prompts.
Malformed, unterminated, unsupported, oversized, missing timestamp and read-error
cases retain partial evidence and typed diagnostics. Unsupported RFC3339 syntax
is not ordered lexically. Chrono supplies timezone-aware parsing.

Root integration request (not edits): tidepool-agent/Cargo.toml add
`chrono = { version = "0.4", default-features = false, features = ["alloc"] }`
matching tidepool-eval's existing dependency. node.rs exports the bounded reader
and neutral UsageSelection/UsageReadLimits/BoundedUsageReport and supporting
report types from rollout_usage. lib.rs exports them and aliases the reader as
`read_native_usage`. Exact declared type names will accompany the candidate.
Consumers follow this committed contract and root's shared wiring baseline;
no raw parser duplicate or temporary successful stub is allowed.

Implementation candidate: 1b7a1aec (followed by focused test/documentation
increment). Shared parser now also validates reasoning <= output and checked
input+output == total; existing and bounded aggregation reuse one total_usage
fold with overflow checks. Response reconciliation occurs before window
selection so a contradictory timestamp across a cutoff cannot create a false
winner. Consequently the response limit counts bound-thread IDs across the
scanned inputs, including IDs outside the selected window; truncation is explicit.

Compilation gate: chrono dependency and public reexports are not present in this
branch because those files remain root/service-owned. No compile/test success
is claimed for this candidate until root supplies that aperture. Exact future
focused command: `just test-lib tidepool-agent 'test(rollout_usage)'`, covering
both existing-owner callers and bounded-reader tests. Then compile and execute
the real run-map consumer after shared exports are incorporated.

Exact shared-export patch for the root/service owner (not yet applied):

```rust
// tidepool-agent/src/backend/codex/node.rs, after mod rollout_usage
pub use rollout_usage::{
    read_bounded_usage, BoundedUsageRecord, BoundedUsageReport, UsageDiagnostic,
    UsageIssue, UsageLimit, UsageProvenance, UsageReadLimits, UsageSelection,
    UsageSourceCoverage, UsageSourceState,
};
// tidepool-agent/src/lib.rs
pub use backend::codex::node::{
    read_bounded_usage as read_native_usage, BoundedUsageRecord,
    BoundedUsageReport, UsageDiagnostic, UsageIssue, UsageLimit, UsageProvenance,
    UsageReadLimits, UsageSelection, UsageSourceCoverage, UsageSourceState,
};
```

The function is implemented at the candidate now; these exports no longer
reference a missing signature. Root may either authorize this exact narrow
wiring in the lead checkout, or commit it with the chrono dependency and return
the baseline. This decision is required before actual target compilation and
consumer integration, not a request to waive verification.
