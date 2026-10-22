# N: one owner for coherent private publication

N consolidates N1 runtime selection and N2 execution/host-write progress.
Primary goal: publish actual private work without losing independent public
progress or fabricating checked completion.
Adjacent questions: cancellation/uncertain commit, retained reply usability,
and old alternate entrypoints that bypass the paired transaction.

Read publication-review.md under g2-peer-review-c910d1-20261021. The N patch
9f77dd17ab is integrated but not accepted. Do not treat the remaining work as
only a package blocker.

## N1: preserve original/private/latest selection

Owned core: codegen BindingTable selection preparation. N owns the joined call
site in runtime/session/persistent.rs; coordinate N2 edits there.

At c910d1, persistent passes latest public source_selection as admitted, so the
equality guard succeeds. BindingTable then derives the installed selection
from completed.selection, the old private snapshot. The unchanged World branch
keeps latest public World. The resulting pair can disagree.

Smallest controlled history:
- Admit native A against W0/S0 and park it before final publication.
- Real public B adds a selected source/domain and matching World W1/S1.
- Finish A with no selected writes and publish; retire A's private scope.
- Require S1/W1 and execute/read B afterward.
- In the negative control, BOTH private A and public B make incompatible changes
  relative to S0 in the same selected domain; require precise refusal. An
  unchanged A must preserve B even when B legitimately replaces an S0 selection.

A scalar-name-only B does not establish the native-selection premise.
Repeat with real checked private progress after the no-write case isolates
the merge owner.

Design the existing owner API around original admitted, private completed and
latest public evidence plus the accepted publication delta. For unchanged work,
preserve latest exactly. For real disjoint private additions, compose exact
owner-issued changes; do not union broad inventory or choose by owner name.
Concurrent conflicting changes must refuse before partial publication.

A simple base replacement is insufficient: today's selection validation
requires every key in source/private custody. Latest-only public keys may not
be there. Validate unchanged public selection against its public owner and
transferred private selection against the actual source/transfer proof.
Carry the result into the SAME prepared promotion used for durable commit and
compiler World projection. Do not validate one prospective state and commit
another.

## N2: progress records real transitions and host values

Owned core: PrivateExecutionAdmission, checked-prefix start/completion,
host mount/acceptance and paired_publication. N resolves shared hunks with N1.

Current classify_private_publication compares final World states at freeze.
RuntimeCheckedPrefix::start already records in_flight elsewhere; the
classification does not observe it. A started first item can therefore look
Unchanged before a successor exists. Normal actor failure routing is helpful,
but the owning publication contract must enforce this itself.

Carry progress at real transitions, using existing checked/host evidence:
- a borrowed unchanged basis that has not started compiler-changing work;
- pending/refused/uncertain work, not publishable as success;
- actual completed checked progress;
- actual retained host-binding/type progress.

These can compose within one invocation. Do not add a boolean supplied by a
caller or infer success from missing position, unchanged logical product,
Tool dispatch, endpoint name or Arc identity. Reuse the real prefix start/end
and host-write owners, not a generic parallel state machine.

The actual retained-output route is:
Pruned after-tool -> bind_tool_result -> mount_prepared_host_input(hidden=false)
-> mount_checked_host_input -> bind_prepared -> PendingHostValueWrite.accept.
It advances bindings/visibility and the completed-values journal but leaves
the private World association stale. Freeze refuses before classification.
The hidden-input refresh has different semantics; routing a visible result
through it or updating only the key would not publish its type fact.

The real host carrier/interface evidence must issue coherent private compiler
progress. Final publication consumes it under the same source transaction and
projects the visible result into the current public World. No fake zero-cell
successor, no broad "native means unchanged", no second host-only commit bypass.

## Production controls and terminal ownership

N1/N2 add focused cases through actual admission/mount/publication. Reuse the
existing runtime/actor framework; no test-only authority factory.

- Unchanged A after selected-source B retains B after private retirement.
- Real checked A with disjoint B composes; actual conflicting selection refuses.
- Installed endpoint with Pruned after-tool retains real Text output, publishes,
  retires private scope, and reads that name in a fresh cell.
- Unpruned counterpart has no spurious retained binding; foreign host carrier
  refuses with no partial visible name.
- Start an actual checked item, attempt freeze before completion: typed refusal.
  Complete a valid history: success. Cover cancelled/failed first item and
  incomplete multi-item prefix. Do not manufacture completion positions.
- If a genuine producer completes checked work without changing semantic World
  identity, test that real case rather than assuming identity implies no work.
- Cancellation before claim, after claim, known pre-rename persistence failure,
  and RecoveryRequired preserve original effects and cleanup obligations.
- Reopening storage while the same machine retains live custody preserves the
  committed host value/type. True cold recovery after machine loss must preserve
  durable publication/provenance and report prior-machine native values as
  unavailable, as declaration_recovery_report does today. A fresh native
  invocation can publish a new value; recovery must not replay the old handler.
  Do not expand this repair into heap persistence. An unfinished private
  operation cannot be authorized by recovery.

Keep existing cancellation claim order and one prepared witness through commit.
Original handler replies/effect receipts survive publication failure; no replay.
Changes must preserve refusal cleanup and unresolved durability semantics.

## Review and integration

N1/N2 may implement parallel named hunks after agreeing the owner API. N is a
working integrator, not a message relay. Compile codegen/runtime/actor production
and affected test targets early. The boolean classifier-table test cannot prove
started/completed ownership; replace its overclaim with real transition controls.

R-N reviews construction, freeze, prepared selection, World projection,
claim/commit and reopen together. Run the four notebook cases with P/W/H at
the joined checkpoint; do not weaken their reload/reply meaning.

Report exact candidate, deleted fallback logic, execution counts and unresolved
boundaries. No newly executed publication history was provided by staff review;
the workers must supply it.
