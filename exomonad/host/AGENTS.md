# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For launch and prompt changes, use focused owning tests such as
`actor_host::prompt_catalog`,
`actor_host::documentation_tests::published_notebook_sources_match_authored_fixtures`,
`actor_host::documentation_tests::published_request_example_retains_success_and_target_cancellation`, and
`exomonad::tests::config_effort_defaults_low_and_preserves_explicit_values` and
`actor_host::cell_model::tests::typed_cell_model_selection_checks_actor_grants_before_provider_work` in the `exomonad-host`
library. When changing launch configuration or tool contracts, update the
scripted-provider fixtures, check exact test selections actually ran, and
compile changed consumers.

For actor-confined notebook policy, source reload, or settlement changes, follow
the [actor verification guidance](../actor/AGENTS.md) for owning compile targets
and the counted notebook cohort.

When a positive progress barrier depends on a tool operation, observe that
exact operation's typed terminal outcome as well as host liveness. Use
`HostedTestRuntime::while_operation_succeeds` for native progress waits;
failure or cancellation must end the wait promptly. Successful settlement
does not replace the requested progress. Browser-provider barriers should
likewise reject an unexpected delivered result instead of waiting for a
second result on an already settled call.

Ordinary input atomically queues text and requests exact continuation through
Store. Observe the admitted receipt's typed continuation outcome separately
from inclusion in a provider request; admission alone does not establish
execution. Retry without new text and explicit visible-history fallback use the
actual `HostInferenceState::Blocked` token and the browser command owner, then
observe its durable `RecoveryAuthorized` receipt before expecting another
provider request. Use `HostedTestRuntime::http_recover_blocked` for those explicit
recovery scenarios. User interruption follows its own cancellation settlement
and must not be treated as a blocked provider failure.

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
Actor retirement, registry removal and final machine release are distinct
observations. On a cleanup timeout, trace the actual remaining request, capture
or value owner and its release contract before moving assertions or extending
deadlines. A controlled backend must settle admitted effects after the typed
cancellation cutoff; holding its answer while awaiting cleanup can block the
cleanup being tested. Enable the owning lifecycle traces first; missing filtered
events cannot establish that cleanup was never attempted.

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

Notebook inspection feedback may be a binding handle. Behavioral assertions
must follow the existing output or inspection owner to the evaluated value;
immediate feedback text is not itself proof of the result. `display` publishes
through the ordinary output coordinator. Hosted tests follow its receipt into
the Store. Component campaigns use `TestCampaign::drive_actor_output` while
awaiting any operation that can publish output, including named authored tool
handlers.

Model-free `TestCampaign` setup that spawns native children must await the
operation through `drive_native_admission`. It acknowledges native installations,
retains their deployment events for assertions, and drives output. Tests of
admission barriers keep explicit acknowledgement ownership.

Recipe activation messages present the typed request; they are separate from
the fresh context seed supplied during provider attachment. Assert payload
fields through the checked actor's `sessionInput`. Assert seeded provider context
at the real provider attachment consumer.
