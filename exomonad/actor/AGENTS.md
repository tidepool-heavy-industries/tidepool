# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Inspect focused tests beside the request, workbench, or lifecycle module. Use
`just test-lib exomonad-actor --exact FULL_TEST_NAME --expected-count 1`; compile changed consumers and
cover relevant failure and cleanup paths as well as successful replies.

For notebook policy, source reload, or settlement changes, compile the owning
consumers with `just check exomonad-actor tidepool-runtime tidepool`, then run
`just test-native //exomonad/actor:actor_notebook_contract_tests`. This counted
integration cohort installs the source-selected notebook policy and checks
acknowledgement, cancellation, reload refusal, uncertainty, and cleanup through
the real actor. Unit-only checks cannot exercise dynamic Haskell preparation or
the installed tool surface.
