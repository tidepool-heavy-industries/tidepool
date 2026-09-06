# Direct numeric-run reconciliation

On 2026-09-06 this lead read the actual directory documented in
`overnight-evidence.md`; raw evidence was not modified. There are 31 actor
incarnation directories, including retrospective actor30. There are 29
per-actor binding files: actor0 instead uses `root-binding.json`, while actor9
has no binding. This is not evidence of 31 simultaneous workers. Inbox records
currently classify as 61 sessionReady, 59 watchChanged, and 25 childExited.
None of these event labels proves review, acceptance, or integration.

A bounded ad-hoc reconciliation selected actors0–29, root's explicit binding,
29 bound provider threads, and the 29 matching rollout files in the indexed
2026/09/06 provider directory. It selected only `token_usage_record`, filtered
payload.thread_id and timestamps strictly before 2026-09-06T10:27:00Z, and
deduplicated (thread_id, response_id) with conflicting usage detection.

Direct result: 905 unique responses; 88,699,459 input tokens; 86,888,192 cached
input; 165,462 output; 32,967 reasoning output (subset). Zero conflicting
usage records or malformed JSON lines in that selection. These reproduce the
index exactly, not independent proof of provider billing completeness or
causal savings. Cumulative token_count records were not summed.

Local evidence: `/tmp/run-map-evidence/reconcile.py` and
`/tmp/run-map-evidence/reconciliation.json`. Script contains paths and logic,
not prompt bodies. This is lead-executed evidence, not fresh independent review.

## Partial product

`cargo run -p tidepool --example run_map -- RUN_DIRECTORY` (repository Nix
environment) is the representative read-only consumer. It reports actor
incarnation directories, bounded inbox event metadata and per-actor bindings,
with JSON stdout and concise stderr. It does not expose assignment messages.
Missing binding remains Unknown. Root linkage uses recorded status identity,
not an actor0 convention. The historical run's status is now Exited and no
longer retains root identity; its separate root thread is observed, but the
actor association remains Unknown. The ad-hoc reconciliation above used its
explicit historical actor selection and is not a reader inference.

The later increment adds UTC Unix-millisecond windows and root binding linkage
when typed status retains exact root identity. Still partial: provider usage via
the existing usage owner, parent/admission/source edges,
structured review/integration/tested revision links and existing Shoal CLI
integration remain outstanding. Reader reports usage/acceptance Unknown rather
than fabricating support. Independent review has now successfully launched and requested local reader
repairs. Historical recursive custody failures remain valid evidence; this
reviewer launch alone does not establish the production custody repair.

## Verification of source db5e4fdc220aedeb4af084712c9082dea6e3128f

- `just test-lib tidepool 'test(partial_map_)'`: 2 executed/passed,
  98 unrelated tests skipped; includes missing binding, torn tail, oversized
  records and actor-count bound. Log `/tmp/run-map-evidence/tests.log`.
- `nix develop --command cargo check -p tidepool --example run_map`: compiled.
- `nix develop --command cargo run -p tidepool --example run_map -- <indexed run>`:
  executed successfully. Nix shell setup adds stdout text, so a subsequent
  invocation of the same built example binary provided clean JSON stdout.
  The JSON parsed successfully: 31 actor directories, 145 inbox events,
  one diagnostic (missing actor9 inbox). Usage and acceptance remain Unknown.
- Binary identity `/tmp/run-map-evidence/example.sha256`; clean JSON
  `/tmp/run-map-evidence/partial-map.json`; summary
  `/tmp/run-map-evidence/partial-map-summary.txt`. No raw prompt bodies emitted.
- `cargo fmt -p tidepool --check` and `git diff --check`: passed.

The Nix stdout prefix caused the first attempt to parse the shell-wrapped
command's output as JSON to fail; it was not a parser failure in the example.
Compile and test commands did not replace the running Shoal host.

## Independent-review repair scope

The reader now retains actor-local inbox I/O failures as diagnostics, including
failure while checking the record limit; root directory enumeration errors still
return an error. Truncated actor selection retains the smallest ordered
(actor, incarnation, path) keys in bounded memory, independent of creation order,
and reports the omission count. Byte limits are validated at entry so the
one-byte oversized-record probe cannot overflow.

Reviewed `tidepool-repr/src/jsonl.rs`: its durable `read_tail` mechanism returns
whole-file rows, has no per-record bound, and treats middle corruption as an
error. This partial inventory needs bounded records and diagnostics while
retaining readable evidence; no new generic JSONL helper or durable log was
introduced in these repairs. Existing JSONL ownership is unchanged.

## Root-claim reconciliation and binding authority

A typed internal RootThreadConstraint retains agreement/absence/conflict before
rendering public Unknown evidence. Known status/root-binding contradiction
makes the exact root node Unknown regardless of whether its per-actor binding
matches status, root-binding, or neither. A missing root binding does not erase
a per-actor claim agreeing with status; a per-actor/status contradiction remains
Unknown. No reason-string branching drives this behavior.

Inspected `tidepool_agent::read_interactive_binding` and its node.rs owner:
it validates the version ladder and thread syntax to construct the opaque
QueueReadyThread readiness proof. The run-map's bounded binding projection
intentionally observes only the recorded nonempty thread text and its path,
including potentially legacy artifacts; it neither constructs QueueReadyThread
nor certifies readiness, lifecycle success or current execution authority.
The map must not be used to launch/control an actor. No binding-version policy
is duplicated here. Root actor identity still uses the existing versioned
RunStatus decoder, with no raw serde fallback.
