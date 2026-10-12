# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For launch and prompt changes, use focused owning tests such as
`actor_host::prompt_catalog`,
`actor_host::documentation_tests::published_notebook_sources_match_authored_fixtures`,
`actor_host::documentation_tests::published_request_example_retains_success_and_target_cancellation`, and
`exomonad::tests::config_effort_defaults_low_and_preserves_explicit_values` and
`actor_host::cell_model::tests::cell_model_policy_matches_admitted_embedded_alias_and_effort` in the `tidepool`
library. When changing launch configuration or tool contracts, update the
scripted-provider fixtures, check exact test selections actually ran, and
compile changed consumers.

When a positive progress barrier depends on a tool operation, observe that
exact operation's typed terminal outcome as well as host liveness. Use
`HostedTestRuntime::while_operation_succeeds` for native progress waits;
failure or cancellation must end the wait promptly. Successful settlement
does not replace the requested progress. Browser-provider barriers should
likewise reject an unexpected delivered result instead of waiting for a
second result on an already settled call.

Ordinary input queues while inference is blocked. Recovery scenarios use the
actual `HostInferenceState::Blocked` token and the browser command owner, then
observe its durable `RecoveryAuthorized` receipt before expecting another
provider request. Use `HostedTestRuntime::http_recover_blocked`; a successful
input receipt does not establish recovery. User interruption follows its own
cancellation settlement and must not be treated as a blocked provider failure.

Host liveness observation must support nested and concurrent barriers. Share
the retained terminal observation of the host's single readiness receiver;
never hold that receiver's mutex across a caller's progress future. Test
barrier composition and cancelled or late observers through this shared owner.

Shutdown tests must distinguish executor termination from physical ownership
release. A retained `Conversation` carries host authority; an `InputObserver`
retains read-only Store evidence. Check late-operation refusal after executor
shutdown, then release execution-owning handles before awaiting the physical
release receipt. Keep read-only observers alive when testing evidence survival;
do not weaken the receipt or discard the evidence to make teardown pass.

Notification tests assert exact message multiplicity, FIFO order and durable
delivery phases. Provider batching may include several admitted notifications
in one request. An idempotent reconciliation may schedule no work when delivery
is already complete; assert the resulting receipts rather than a fixed task or
provider-round count unless that count is the API contract under test.

Rich views and human forms share `FormHost`, installed before actor admission.
A form-unavailable campaign that still publishes views needs the production
display delegate and an explicit typed refusal at form opening.
Optional notebook effects derive from installed services at admission. For an
unbound-conversation case, install a reader that returns the typed unbound result
through the existing conversation seam so the root admits `Reflect`.

Recipe activation messages present the typed request; they are separate from
the fresh context seed supplied during provider attachment. Assert payload
fields through the checked actor's `sessionInput`. Assert seeded provider context
at the real provider attachment consumer.
