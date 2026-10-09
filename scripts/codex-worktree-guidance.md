# Shared worktree-agent contract

The parcel defines the task. Follow this file and the nearest contributor
rules; a narrower parcel constraint wins. Preserve unrelated work and WIP.
Do not switch another active checkout, stash/reset edits, clear shared caches,
or restart shared services. Each implementation owner runs its own affected
builds and focused tests through the admitted repository commands. Run independent
work concurrently when measured process peaks, enclosing cgroup limits and host
headroom permit; coordinate actual checkout, configuration and output conflicts.
See `docs/swarm-builds.md` for admission and provisioning. A missing output mount
or compiler input is a concrete blocker, not a reason to make source-only handoffs
the default.

Before adding an abstraction, find its production callers and extend the
owning mechanism. A test-only caller is not evidence for new public surface.
Review unexpected complexity as a finding, then repair it within scope.

## Focused verification

1. Map every changed file and direct consumer to its declared target. A shared
   type, serialization shape, generated schema or command can affect more than
   the defining crate. Cargo metadata and the reviewed source/module walk own
   Rust target registration; Cabal component declarations own Haskell rosters.
2. Compile every changed/directly affected target in the admitted checkout.
   Compile test targets and integration callers of changed APIs early, while
   independent source review proceeds; do not wait for a central build owner.
   Linking a Rust test executable is compile-only evidence. Actual libtest
   discovery, not source regexes, decides whether a harness is an executed
   suite. Empty harnesses cannot be accepted as tests.
3. Run the smallest exact cases that prove each important success, refusal,
   recovery and cleanup path. Use the declared runner's runtime resources;
   missing required compiler inputs must fail, rather than silently skip.
4. Reuse exact-source checks unless changes invalidate them. After a repair,
   rerun the failing case and affected consumers, rather than every prior gate.
5. Run appropriate formatting/source checks and `git diff --check`. Review for
   stale callers, duplicate owners, unused public surface and string-driven
   control flow. Report unqualified targets explicitly.

Each Buck checkout needs its own provisioned `buck-out` bind mount on
`/srv/build`. Prepare its selected pinned Nix outputs through configure, which
retains an inspectable GC-root generation before publishing checkout config;
see `docs/swarm-builds.md`. Keep remote execution disabled until its closure,
isolation, reuse and cancellation gates are accepted. The first Buck query
also starts a daemon, so it belongs inside the admitted build slice.

## Selection and evidence

```sh
just test-lib PACKAGE --exact FULL_TEST_NAME --expected-count 1
just test-target PACKAGE TARGET --exact FULL_TEST_NAME --expected-count 1
just test-native //bridge/haskell:SUITE --pattern PATTERN
just build //path:TARGET                 # compile-only
```

`just` selects declared labels and forwards to the owning runner. Native
Rust runners preserve runtime resources, discover real names, reject zero or
wrong counts and run each case in a bounded process. Use `--output-dir` to
retain runner evidence. Haskell suites use their shared Tasty runner. Ignored
compiler packet command adapters expose producer-only RunInfo; invoking them
is not independent test acceptance. Standard Python regression targets also
reject zero selected cases.

For delegated cgroup tests, keep Buck and the Python runner outside the
service; only the actual test executable enters its delegated service. The
runner records exact service admission and cleanup. Do not count an unknown
launch or an absent unobserved unit as confirmed cleanup.

Retain source OID, actual artifact path/hash, exact command, toolchain/profile,
selected and executed counts, exit status, elapsed boundaries and logs. Name
what **passed**, what **compiled without execution**, and what remains
**unqualified**. A failed baseline control is **executed, failed as expected**;
keep observation separate from expectation. Listings, source generation,
unknown counts and zero-test exits do not close behavior obligations.

Do not infer cold/warm speedups from unrelated timings or clear shared caches
to manufacture a cold run. Distinguish environment/tool fetching, compilation,
producer actions, actual execution and cleanup. Never summarize a neighboring
target's success as proof for all changed files.

## Release identity

Build the native runtime bundle, then freeze it with the central owner in
`build/package/qualification.py`; see its README. A frozen bundle owns the
same-profile host/libtest, compiler pair, deployment, stdlib, assets and a
build-side source/artifact contract verified against clean tracked Git and
recorded submodules. Run/init/acceptance require the explicit descriptor.
Do not provide an arbitrary libtest/compiler path or infer a bundle from raw
Buck output. Preserve the same frozen files across acceptance and live runs.

Keep open qualification/migration obligations in the approved plan. Remove
completed handoff prose after its current invariants live in owning docs;
Git retains checkpoint history.
