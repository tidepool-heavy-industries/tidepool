# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For launch and prompt changes, use focused owning tests such as
`actor_host::prompt_catalog`,
`actor_host::documentation_tests::published_notebook_sources_match_authored_fixtures`,
`actor_host::documentation_tests::published_request_example_retains_success_and_target_cancellation`, and
`fork_effort_defaults_low_and_preserves_explicit_overrides` in the `tidepool`
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

Host liveness observation must support nested and concurrent barriers. Share
the retained terminal observation of the host's single readiness receiver;
never hold that receiver's mutex across a caller's progress future. Test
barrier composition and cancelled or late observers through this shared owner.

Rich views and human forms share `FormHost`, installed before actor admission.
A form-unavailable campaign that still publishes views needs the production
display delegate and an explicit typed refusal at form opening.
Optional notebook effects derive from installed services at admission. For an
unbound-conversation case, install a reader that returns the typed unbound result
through the existing conversation seam so the root admits `Reflect`.
