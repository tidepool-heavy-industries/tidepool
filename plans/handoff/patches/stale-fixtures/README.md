Stale fixture / test-contract drafts against HEAD ddb9d2746 (engine/stg-production-cutover).
Nothing was applied, committed, or built. Each patch passes `git apply --check`
individually and all together. `work/b/` holds the post-patch copies.
None of these were compiled or run (a background chain owns the build).

01-request-assignment-fixtures.patch
  Tests: roster_observation_preserves_host_and_sibling_workbenches,
         forest_operator_survives_model_root_recovery,
         active_update_keeps_original_request_and_fences_terminal_delivery,
         notification_admission_and_poll_preserve_typed_request_bindings
  Files: tidepool/src/actor_host/{roster_setup,operator_request,active_update_setup,notification_setup}.hs
  Change: bbf724ae1 made `request :: AgentRef -> Assignment input -> Eff effs (Response result)`
    (haskell/actors/Tidepool/Actors/Internal/Agent.hs:272). The label and input are now bundled.
  Edit: `request @T w label x` becomes `request @T w (assignment label x)`, which is the
    form already used in quiet_observation_setup.hs and documentation_tests.rs.
  Intent kept: the same typed request, with the same label, input, and target, is created.
    The downstream assertions are unchanged.

02-message-actor-agent-ref.patch
  Test: haskell_actor_sends_normal_steering_without_a_native_session
  File: tidepool/src/actor_host/message_actor.hs
  Change: `sendMessage :: AgentRef -> Text -> ...` (Agent.hs:640). bbf724ae1 removed
    MessageRecipient and added an ambient `me :: AgentRef`
    (resident_workbench.rs:69; SHOAL doc: "`me` is the current actor's exact address").
  Edit: `owner <- actorContext` becomes `let owner = me`, and the definition is typed
    `(AgentRef, Text)` (AgentRef is exported by Tidepool.Actors.Shoal).
  Intent kept: root captures its own address and hands it to a Haskell actor, which
    steers root. The test still asserts target == root, owner != root, the exact
    message, and that no model session is launched.

03-workbench-doc-assertion.patch
  Test: hosted_lookup_and_status_use_actor_owned_views (tidepool-actor)
  File: tidepool-actor/src/hosted_lifecycle_tests.rs
  Change: 57def9e0a replaced the line-unit guide with notebook cells.
    prompts/shoal/docs/workbench.md now opens with "Send one notebook cell of ordinary Haskell".
  Edit: the needle is swapped for that opening sentence.
  Intent kept: `doc workbench` resolves to the workbench guide body, as distinct from the
    `doc` index, the unknown-topic item, and the signature item in the same mixed query.

04a-PRODUCT-active-children-render.patch   <-- PRODUCT CHANGE, flagged
  Test: actor_workspace_recipes_distinguish_orchestrators_from_coding_workers
  File: tidepool/src/actor_host.rs (append_effective_role, and the test)
  Finding: the product is wrong, not the test. 1bdc55c7c wrote `active_children={}` for a
    u16 and asserted `active_children=3`. 99f53a493 made the field `Option<u16>` and
    switched the format to `{:?}` mechanically, updating the test's input but not its
    expectation. The model-facing developer prompt has rendered `active_children=Some(3)`
    (or `None`) ever since, which is Rust Debug leaking into model text.
  Edit: render `3`, or `unbounded` for None (role.rs: "None leaves concurrency
    unbounded"). The original assertion is kept, and an assertion that no `Some(`
    appears is added.
  Not patched, same leak: runtime_observation.rs:164 `max_active_children={:?}` (launch
    orientation, also model-facing) and resident_actor.rs:1028 `active_children={:?}`
    (status view). Consider one shared rendering.

04b-hosted-tool-order.patch
  Test: frozen_tools_dispatch_raw_and_structured_inputs_without_workbench_bindings
  File: tidepool/src/actor_host/hosted_tools_tests.rs
  Change: ResidentInteractivePolicy::local_with_tools (tidepool-actor/src/resident_interactive.rs:33)
    now declares [haskell, lookup, status, ...project tools]. lookup and status are
    HostedTool::Function. They arrived with 7cf4681c7/57def9e0a (hosted lookup/status).
  Edit: assert indices 1 and 2 are lookup and status, and 3 and 4 are raw_echo and repeat_text.
  Intent kept: frozen project tools are projected with the right kinds (Custom raw,
    Function structured), and the core-tool ordering is now pinned explicitly.

04c-uncertain-descendant-retire.patch
  Test: lost_descendant_custody_does_not_authorize_parent_reclamation
  File: tidepool/src/actor_host/overlay_resource.rs (test only)
  Change: the behaviour change is e0312490c ("retain uncertain custody"), not a78fc7fec.
    retire() no longer errors on uncertain inherited custody. It marks the storage
    RetainedUnconfirmed and skips storage.release(). a78fc7fec only added the comment
    "it vetoes reclamation, not retirement of this owner's view".
    2c9fdc559 had recorded this test as a pre-existing baseline failure.
  Edit: `retire()` must succeed, the published snapshot is cleared, and `parent_path` still exists.
  Intent kept: lost descendant custody does not authorize reclaiming the parent's
    storage. The directory-survives assertion is the reclamation check and is unchanged.

04d-quiesce-completion-boundary.patch
  Test: http_quiesce_preserves_completion_and_drain_retains_blocked_call
  File: tidepool/src/host_dynamic_tools_drain_tests.rs
  Change: 4026f4856 made /call insert an Active boundary for its contextCallId, and
    /completed refuse Active/Reconciling/Pending boundaries (409 at first). 3b65def78
    changed that refusal to a retryable 503 "still settling; retry acknowledgment".
    The test completes contextCallId "context", which is the very call still blocked
    in flight, so it now gets 503.
  Edit: during quiesce each client completes a non-in-flight boundary
    ("retired-context"), which must return 200. It then attempts the in-flight
    "context", which must return 503 with "still settling". The existing counts
    `completions == 2` and `calls == 1` now prove the settling attempt was not
    delivered to the endpoint.
  Intent kept: quiesce still admits completions (the 200s and the endpoint count),
    denies new calls and sessions, and drain retains the blocked call. The designed
    boundary refusal is also covered.

04e-operator-whole-cell-typecheck.patch
  Test: operator::tests::unix_http_live_workbench_and_graph
  File: tidepool/src/operator/tests.rs
  Change: 57def9e0a typechecks the whole cell before any effect, so
    `let committedPrefix = 42\nmissingName\n...` now installs nothing
    (workbench.md: "Typecheck rejection installs no bindings"). Committed prefixes are
    retained for runtime failures only.
  Edit: (1) a type-error cell is Rejected and its `let` stays unbound (new contract).
    (2) the committed-prefix check uses a cell that typechecks and fails at runtime
    (`if error "..." then ...`, the pattern from command_prefix_failure.hs). The prefix
    must be Completed and `neverRuns` must be unbound.
  Intent kept: operator HTTP rejection preserves the committed prefix and does not run
    the suffix.
  Assumption: a runtime-failed cell has WorkbenchRunStatus::Rejected, so Outcome::Rejected
    via operator.rs map_result. I have not run this; if the cell reports a different
    status, adjust only that one assert.

04f-display-failure-fixture.patch
  Test: failed_command_display_retains_result_without_reexecution
  File: tidepool/src/actor_host/command_display_failure.hs
  Change: 57def9e0a added automatic structural Display for ordinary `data`
    declarations. That beats the OVERLAPPABLE `Show a => Display a` instance
    (Tidepool/Inspection.hs), so the erroring `Show` instance is never called and
    display succeeds. workbench.md: "Explicit instances are preserved."
  Edit: author an explicit `instance Display BrokenDisplay where displayTree _ = error ...`.
    The import form is copied from notebook_display_custom.hs. displayWith defaults
    through displayTree, so every render path hits the error.
  Intent kept: a failing display still yields "Display failed" / "Value remains bound"
    (resident_workbench.rs:2575). The retained value is still usable
    (`Cmd.stdout (savedResult broken)`) with one backend launch. The test itself is unchanged.

Refused / not patched: none of the listed groups. 04a is a product fix rather than a
test edit, and needs sign-off because it changes model-facing prompt text.
