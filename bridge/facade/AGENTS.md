# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

For launch and prompt changes, use focused owning tests such as
`actor_host::prompt_catalog`,
`shared_api_guide_example_handles_success_and_unavailable`, and
`fork_effort_defaults_low_and_preserves_explicit_overrides` in the `tidepool`
library. When changing launch configuration or tool contracts, update the
scripted-provider fixtures, check exact test selections actually ran, and
compile changed consumers.
