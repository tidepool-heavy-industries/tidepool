# Independent partial run-map review (pending repairs)

Candidate fd6f1358d7f5b488d70f1cca6ef7caea661e6984 inspected at reviewer merge
9884a7b6caaf6d8120e41f15f50165ab6ec8e18b. This is review of partial inventory,
not full run-map acceptance. No repaired candidate accepted yet.

Requested direct implementer repairs through retained typed request mapRepair:
- Per-inbox read failures currently propagate out of read_run, losing the partial
  report. A directory at inbox.jsonl supplies deterministic Linux read failure.
- Truncation selects filesystem-order actors before sorting; creation order can
  change selected nodes. Use bounded deterministic selection and test it.
- bytes_per_record plus one can overflow for maximal public Limits input.
- Make current review status distinct from historical recursive launch failures.

The shared tidepool-repr JSONL reader enforces journal corruption semantics;
run-map needs read-only bounded inventory with malformed-record diagnostics.
Any new general parsing mechanism belongs there; this review does not demand
using unbounded journal collection or expanding production scope reflexively.

Reviewer directly inspected the lead's local reconciliation script: bound threads
selected from actors 0–29, root binding special case, per-response/thread dedupe,
conflict detection and exclusive cutoff; no cumulative token_count summation.
Reviewer independently reran that existing script (not an independently authored
algorithm): 29 files/threads, 905 responses, zero conflicts or malformed lines;
input 88699459, cached input 86888192, output 165462, reasoning output 32967.
Output /tmp/run-map-review/reconciliation.json. Script SHA256:
edf385ce2791a8642121fd97e5b00b0f482d84e5d162f637134de7cecacbeeff.
This reproduces selected historical totals, not billing completeness, shipped
usage implementation, price/cost superiority or causal cache savings.

UX: activation's opaque structured assignment was usable with fst/snd immediately;
no API inventory required. Native branch check confirmed reviewer checkout despite
inherited root dialogue. Repair request/watch needed only documented operations.
This successful reviewer launch does not waive the earlier custody failures.
Focused Rust verification is deferred to revised candidate to avoid redundant
expensive builds; current source inspection is not behavioral evidence.
