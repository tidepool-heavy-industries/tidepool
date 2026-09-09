# Typed routing usage review

This pass keeps the actor kernel and normal TUI execution model. It replaces
attention-only project collectors with one `Project.Routing.followWork` actor per
local wave. Native execution and engine product changes remain on their branches.

## Behavior

- Retain evidence, per-source questions, closure and complete terminal response
  receipts together. Query compact snapshots; expand the original values as needed.
- Select recipient-specific messages in Haskell. Default messages carry changed
  questions, resolutions, failures and final outcomes. A checkpoint decorator adds
  useful partial commits without dropping simultaneous question changes.
- Preserve send failures and uncertainty as data alongside the observed event.
  Replacement retains that state and does not retry uncertain sends.
- Forward actual `WorkEvent Delivery` values through typed subtree mailboxes.
  An already authorized finite continuation can commission review from an available
  retained reviewer and attach a collector without a model relay turn. Blocked or
  unavailable implementation retains its original receipt instead of starting review.
- Capture the Sol owner's context before creating a router. `sendMessage` accepts
  that address through the existing authority-checked inbox. Poll a notification
  through its issuing router; copied receipts and replacement incarnations do not
  inherit the old sender's authority.
- Retire incorporated wave collectors after assigning remaining obligations;
  keep useful native workers independently. Prompts and examples use these paths
  directly rather than appending another bookkeeping checklist.

## Verification

Artifacts live under `target/routing-usage-review-20260909/` in the primary
repository. The fixed `final-main/bin/shoal` and copied `final-main/workspace/.shoal` passed
static compilation with package identity
`8be14e9d2925cc984c98af598bc4c6e143a72b1549a67b988c5f7579c56d05b1`.
`final-main/selection.json` records exact source, binary, package and evidence hashes.

Focused resident recipe execution covers notification failure/uncertainty retention,
automatic retained review, independent progress sources, both later lane Git
handoffs, workbench operations and collaboration. These are separate selected
invocations, not a claim that the entire default recipe battery ran on one snapshot.
The final message-delta check specifically exercises same-source amendment, source
advance, actual resolution and unchanged questions.

Native resident/inbox checks cover captured-context steering, the shared guide,
request-bound notification admission/polling, and router-owned receipt queries.
The receipt test verifies rejection of direct parent polling and old-receipt polling
after replacement, while retaining the original send evidence. The migrated module
and progress-consumer checks cover removal of the obsolete project helper.

All six focused native checks passed:

- `haskell_actor_sends_normal_steering_without_a_native_session`
- `shared_api_guide_example_handles_success_and_unavailable`
- `notification_admission_and_poll_preserve_typed_request_bindings`
- `work_router_queries_receipts_as_the_issuing_actor`
- `work_actor_consumes_later_progress_without_rearming`
- `workspace_recipe_modules_and_snapshot_helpers_compile`

The focused recipe paths completed 60 assertions:

| Path | Assertions | Retained log |
|---|---:|---|
| Notification retention and uncertainty | 9 | `final.log`, initial completed section |
| Automatic retained review | 5 | `review-delivery.log` |
| Independent progress sources | 6 | `callers.log`, completed section |
| Later final Git handoffs from both lanes | 8 | `callers.log`, completed section |
| Workbench operations and prompt freezing | 11 | `callers.log`, completed section |
| Question amendments, source advance and resolution | 1 | `question-deltas.log` |
| Collaboration, decision propagation and uncertain steering | 20 | `collaboration-fixed.log` |

`final.log` and `callers.log` establish only the completed sections listed above;
their overall invocations failed in later examples. Those repaired paths passed
separately in `review-delivery.log` and `collaboration-fixed.log`. The final copied
package passed static compilation; the entire default battery was not repeated.
The Shoal binary build, Rust formatting and staged diff checks also passed.

No providers or native model workers are launched by these checks. Live throughput,
token savings and the two product megatasks remain outside this acceptance.
