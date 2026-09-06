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

## Reviewed repair and independent verification

Accept repaired candidate 2116c9c2a57a5cb7fcf470ec53fbfe236ab92957 for the
explicitly partial inventory contract only. Reviewer merged it at
c3c796dc0fbfe3cd49b3ca64397a3442d868fb20, inspected all repairs and independently ran:
- `just test-lib tidepool 'test(partial_map_)'`: 5 executed/passed, 98 skipped;
  `/tmp/run-map-review/repaired-tests.log`. Test summary 0.008s; Cargo test build
  4m29s plus extractor/bootstrap setup. Daemon teardown explicitly reported.
- `nix develop --command cargo build -p tidepool --example run_map`: compiled;
  `/tmp/run-map-review/repaired-example-build.log`.
- Direct built example on indexed historical run plus JSON assertions: passed,
  31 actors and 145 events; `/tmp/run-map-review/repaired-map.json` and summary.
- `cargo fmt -p tidepool --check`, `git diff --check`: passed.

Example SHA256 bc769fb10f41804da09df83f11b9aa68910c447192b680441f7f27e882f2029f.
BTreeSet retains deterministic smallest keys with bounded memory (full directory
enumeration still required); local inbox errors preserve partial evidence; byte
probe addition is checked at owning entry. Requested regressions executed.

No custody/service acceptance, integrated usage, optional window, root-binding
linkage, graph edges or CLI integration is implied. Parent integration is separate.
Current review supersedes the pending status above; earlier paragraphs retain the
specific original findings and repair provenance.

Additional UX/performance observation: a five-test pure Rust selection rebuilt a
fresh extractor and broad dependency closure in this isolated reviewer checkout,
while selected test execution took 0.008s. This directly illustrates setup overhead,
not proof of a particular optimization's savings. Narrow build/setup reuse merits
measurement without weakening checkout isolation or matched-toolchain policy.
The direct retained request/watch repair loop worked without API rediscovery.

## Window/metadata follow-up review

New candidate 78f7b365a76bf0186cee97886b83f0d601ca4fcf incorporated by fast-forward
on reviewer branch. Reviewed owning status decoder and real TypedActorEvent /
WatchNotification producers: UTC timestamp is only claimed where recorded;
request/watch projections omit assignment prose. Status decoder version gate is
reused, with unsupported-version regression. Declared graph/usage holes remain.

Requested repair: status/root binding conflict became Unknown at report.root but
could leave the known root actor node with an Observed per-actor thread. Require
conflict propagation to that node with typed internal evidence, not reason-string
matching. Distinguish absence from contradiction. Waiting on retained
windowRepair/windowRepairReady; no acceptance yet. Final focused verification will
run against the repaired exact revision.

## Window/metadata repair accepted

Accept increment 0c5623f1c6c3a523598f3f9f337b14558c97ea74 within declared partial
scope. Reviewer integrated it at 63eab9622c753124d52ccd366a2656c1b83ea204 and
independently inspected typed RootThreadConstraint propagation: contradiction
cannot be overridden by per-actor evidence; absence is distinct. Raw bounded
binding thread projection is artifact evidence only, not QueueReadyThread proof.
The owning RunStatus decoder remains the sole status-version gate.

Independent checks at that exact reviewer merge:
- `just test-lib tidepool 'test(partial_map_) | test(run_map_)'`: 9 executed/passed,
  99 skipped, compile daemon torn down; /tmp/run-map-review/window-tests.log.
- `nix develop --command cargo build -p tidepool --example run_map`: compiled;
  /tmp/run-map-review/window-example-build.log.
- Built example exercised with temporary sanitized fixtures: unbounded, from-only,
  until-only, inclusive/exclusive, empty windows; reversed window, missing argument,
  malformed integer rejected. Five success and three failure-path assertions passed;
  assignment marker absent from stdout/stderr. Output /tmp/run-map-review/window-cli-checks.log;
  inline harness retained in reviewer native-tool receipt.
- cargo fmt -p tidepool --check and git diff --check passed.
Example SHA256 9f767742790b18b4c9851628976f266f8242c277f2cca3eeeb0fece3d50351c9.

Usage, unrecorded graph edges and root-owned Shoal CLI integration remain outside
this increment. No service/custody acceptance claimed. This warm reviewer rebuild
reported 12.44s Cargo test build and 0.007s selected test execution, versus the
previous cold checkout 4m29s build: different source/cache conditions, not a
controlled causal speedup claim. The retained reviewer/implementer repair loop
again worked without rediscovering Haskell orchestration APIs.

## Owning Shoal CLI integration review

Accept exact CLI candidate 9374fbe80aaa02402744771fa32f3c24464aac38. Reviewer
fast-forwarded to that revision and inspected its sole changed file,
tidepool/src/bin/shoal.rs. The RunMap branch directly calls the accepted reader;
no host initialization, tracing, native installation or attach path is invoked.
Clap owns directory/window parsing and help; the reader validates bound ordering.
JSON and concise outputs remain separate. No unchanged reader re-review needed.

Independent checks at 9374fbe80aaa02402744771fa32f3c24464aac38:
- `nix develop --command cargo test -p tidepool --bin shoal`: 2 executed/passed,
  no ignored/filtered tests; /tmp/run-map-review/cli/tests.log.
- `nix develop --command cargo build -p tidepool --bin shoal`: compiled;
  /tmp/run-map-review/cli/build.log.
- Inspected root's /tmp/root-run-map-cli/check.py then independently reran it
  against reviewer-built binary: 8 invocations passed (5 JSON windows, 2 rejection
  paths, concise output), temporary HOME unchanged and empty PATH required no
  external tools; /tmp/run-map-review/cli/check.log. This is independent execution
  of the inspected root harness, not independently authored test logic.
- cargo fmt -p tidepool --check and git diff --check passed.
Reviewer binary SHA256 b823337616b8c71fe14df71aec4b00da83ba9c00e9b1b21851c34fd00d42f413.
This differs from root's reported binary hash; both identities remain separate.
No identical-artifact/reproducible-build claim is made. Runtime host was not launched
or replaced. Reader tests remain root-attributed for this CLI-only review; they
were independently checked in the preceding window increment review.

No actionable CLI defect found within scope. Usage parsing and service/custody
remain separate outstanding obligations.
