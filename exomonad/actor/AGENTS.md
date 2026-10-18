# Contributor workflow

For this crate's ownership boundaries and invariants, see [CLAUDE.md](CLAUDE.md).

Inspect focused tests beside the request, workbench, or lifecycle module. Use
`just test-lib exomonad-actor --exact FULL_TEST_NAME --expected-count 1`; compile changed consumers and
cover relevant failure and cleanup paths as well as successful replies.

Stateful fixtures must make their established lifecycle state explicit. Check a
helper's production calls and postconditions before repeating admission,
publication, or settlement in its consumer. A test of a transition starts before
that transition; a test of later behavior reuses the already-issued result.
When a fixture migration breaks this contract, inspect its other callers and
keep a real-owner control proving that a duplicate transition refuses without
replacing the original result or preventing subsequent valid use. Do not make
production transitions permissive to compensate for duplicate test setup.

For private notebook policy, source reload, or settlement changes confined to
this crate, compile the actor's test configuration and the hosted consumer:

```sh
just build //exomonad/actor:exomonad_actor_unit_tests \
  //exomonad/host:exomonad_host //exomonad/host:exomonad_host_unit_tests
```

The normal actor and runtime libraries build transitively. Changes to public
interfaces, upstream runtime or facade code, host CLI code, or integration tests
also require their affected consumer checks.

Then run `just test-native //exomonad/actor:actor_notebook_contract_tests`, which
builds the actor integration executable and executes four cases. This counted
integration cohort installs the source-selected notebook policy and checks
acknowledgement, cancellation, reload refusal, uncertainty, and cleanup through
the real actor. Unit-only checks cannot exercise dynamic Haskell preparation or
the installed tool surface.

The named notebook cohort already supplies its exact selectors. To investigate
one case, use `just test-target exomonad-actor actor --exact FULL_TEST_NAME
--expected-count 1`; do not append another selector to the cohort command.
Runner argument rejection is zero executed tests, not a test failure.
