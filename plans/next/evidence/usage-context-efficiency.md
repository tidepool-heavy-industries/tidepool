# Usage lead context-efficiency follow-up

Incorporated root baseline `5a7b21ae5b7f0d03b024f170bba54dcf774addf1`
by fast-forward merge, preserving prior history. Direct rerun in inherited Nix
shell: both runghc consumers with `-Wall -Werror` passed (14 synthetic cases and
historical survey assertions). This is incorporation evidence, not service proof.

## Observations and narrow response

- Root's retained observation: watch documentation omitted `requestOptions`;
  constructor discovery led to one rejected input. Fix in `e4267216` supplies
  exported smart-constructor use, distinct progress/result types, labeled progress
  and terminal-result watches, and explicit one-way/steering limits. Source
  inspection confirms `requestOptions` is exported by Shoal and the underlying
  authored `requestWithProgress` order is progress, result, input. Live `:type`
  queries confirmed callable values; executable documentation test is separate.
- Helper specialist retrospective (attributed): duplicated revision, binary and
  evidence-path metadata across two manually constructed checks. Small response:
  use a task-local partial application capturing verified metadata when a report
  repeats it; no public constructor or campaign framework is needed.
- Recipe specialist retrospective (attributed): no Haskell discovery friction;
  inherited delivery constructors sufficed. Native environment/cleanup failures
  did not establish a Haskell API gap.
- TL firsthand: separate retained watches allowed recipe integration while helper
  repairs continued. One delayed recipe-ready notice concerned an already handled
  result; polling confirmed its identity without resubmission. Existing watch
  documentation already addresses duplicate notices; no additional wording needed.

These observations identify avoidable discovery and manual duplication, not
measured token savings, cache preservation, or tree-versus-serial efficiency.
Retrospectives were newly queued after original obligations settled, not silent
replacement amendments. The doc fix cannot repair broken transport or provide
an escalation handle. No notification/runtime changes or broad prompt rewrite.

## Reviewed and integrated verification

Independent reviewer accepted documentation commit `e4267216` and supplied
executable test/fixtures in `3db368f7b38ce8947057702b297d0f3885d5b03b`.
The test extracts the actual unique example, checks `[Text]` progress while final
`Text` remains pending, then settles and verifies retained progress and cleanup.
No documentation repair was requested. TL inspected the concrete test and merged.

TL directly executed at integrated revision
`64ce58ab5f3741a3f2ac4ef66b3fcda5eb54371d`, in repository Nix environment:

```
just test-lib tidepool 'test(watch_documentation_request_options_reports_progress_then_settles) | test(shared_api_guide_example_handles_success_and_unavailable) | test(actor_host::prompt_catalog::)'
cargo fmt -p tidepool --check
git diff --check
```

ExecutedPassed, expected passing: selected/executed 6, passed 6, 93 excluded by
filter; compile daemon teardown logged. Test target compiled; all new fixtures
executed through actual resident actor/compiler paths. Initial target build
4m26s; test execution 57.448s. Uncontrolled setup/cache conditions, not a savings
claim. Reviewer independently executed the same six tests on its candidate;
that run's report is attributed here and distinct from TL's direct rerun.

SHA256 identities at integrated test revision:
- lib-test `tidepool-77541008b95d8df9`: `53114c2e9d79eade17a43c8e3a3a04781f9f088bd8df0f349741fdb2f1667b0b`
- Rust extractor: `463d2664aea5b9e776efacd1ed7d1659735998caf340c17c676cb375401e2c93`
- worktree-local GHC 9.12.2 worker: `0baeb3d9b9fe5924a8d3edd09ed85ed536f4653f1dfed1c4fe3fcf6072b64b41`
- full local `/tmp/usage-progress-integrated.log`: `abf3d454328555f31e63cd30e55f71d058217727cf5e513194c946c06e5e3bba`

Reviewer firsthand retrospective: no Haskell discovery calls were needed for the
new test because adjacent executable examples supplied the types. Its earlier
review found an evidence-origin mismatch by tracing the consumer, not by API
inventory. No further public API or broad shared-prompt change is justified.

Limits: local resident tests are not native provider/controller/TUI acceptance.
No host replacement, notification/runtime change, token/cache measurement or
realistic future-user discovery improvement measurement occurred. Final evidence
commit changes only this report; root integration still requires its own decision.
