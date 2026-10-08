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

Rich views and human forms share `FormHost`, installed before actor admission.
A form-unavailable campaign that still publishes views needs the production
display delegate and an explicit typed refusal at form opening.
