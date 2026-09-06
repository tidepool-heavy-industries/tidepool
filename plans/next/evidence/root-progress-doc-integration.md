# Root progress-documentation integration

Accepted independently reviewed usage candidate 653cc396e8f192afc2dd9d0d4c34b41e74845b50.
Integrated and directly tested revision: 551adab1b663d1620a3df1f3fe82c8386815b89c.
Root inspected actual doc, extracted-snippet test and fixtures, then ran:

`just test-lib tidepool 'test(watch_documentation_request_options_reports_progress_then_settles) | test(shared_api_guide_example_handles_success_and_unavailable) | test(actor_host::prompt_catalog::)'`

ExecutedPassed, expected passing: 6 selected/executed/passed, 98 skipped.
Cargo test build 10.73s; test execution 46.017s; per-run daemon teardown reported.
Evidence: `/tmp/root-progress-doc-evidence/integrated-tests.log`.
Test binary `.shoal/build/cargo/debug/deps/tidepool-77541008b95d8df9` SHA256:
13de58223b599ddfc85925b644e35aa04a716bd3c3fc86f27c02a39f82c1b1a4.
`cargo fmt -p tidepool --check` and `git diff --check` passed, checkout clean.

The actual documentation example now proves smart-constructor usability, distinct
progress/reply types, progress while reply is pending, terminal reply and retained
progress. This is executable regression evidence, not a measured reduction in
future agents' discovery tokens. No default API-guide expansion was necessary.
The running host was not rebuilt/replaced; compiled-in documentation on this live
host may remain old until a user-controlled restart onto a rebuilt host.

Existing artifacts were retained between root integration checks, so this shorter
build is not an isolated controlled estimate of any build-cache optimization.
No native provider, custody or controller/TUI acceptance is claimed.
