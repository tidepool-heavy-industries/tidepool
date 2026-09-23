set shell := ["bash", "-euo", "pipefail", "-c"]
nix := "bash scripts/dev-shell.sh"
exomonad_nix := "bash scripts/dev-shell.sh --exomonad"

# Show the supported development workflow.
default:
    @just --list

# Extractor-free engine unit tests; does not build or start GHC.
quick:
    {{ nix }} scripts/quick.sh

# Formatting check and strict clippy over every target, reporting all errors.
lint:
    {{ nix }} scripts/lint.sh

# Lint and the default-filter test tier as independent steps; both always run.
check:
    {{ nix }} scripts/check.sh

# Run one GHC-heavy crate, optionally restricted by a nextest filter expression.
[positional-arguments]
test crate filter="":
    #!/usr/bin/env bash
    args=(-p "$1")
    if [[ -n "$2" ]]; then
      args+=(-E "$2")
    fi
    {{ nix }} scripts/battery.sh "${args[@]}"

# Build and run only one integration suite (test files remain separate modules).
[positional-arguments]
test-target crate target filter="":
    #!/usr/bin/env bash
    args=(-p "$1" --test "$2")
    if [[ -n "$3" ]]; then args+=(-E "$3"); fi
    {{ nix }} scripts/battery.sh "${args[@]}"

# Build and run only a crate's unit-test target.
[positional-arguments]
test-lib crate filter="":
    #!/usr/bin/env bash
    args=(-p "$1" --lib)
    if [[ -n "$2" ]]; then args+=(-E "$2"); fi
    {{ nix }} scripts/battery.sh "${args[@]}"

# Test shared extractor resolution and daemon lifecycle without compiler builds.
test-toolchain-scripts:
    {{ nix }} python3 scripts/tests/test_lib_extract.py -v

# Start (or reuse) the persistent compile daemon; battery/suite/check reuse it automatically.
daemon-start:
    {{ nix }} bash -c 'set -euo pipefail; source scripts/lib-extract.sh && resolve_tidepool_extract && daemon_start_persistent'

# Stop the persistent compile daemon started by `just daemon-start`.
daemon-stop:
    {{ nix }} bash -c 'set -euo pipefail; source scripts/lib-extract.sh && daemon_stop_persistent'

# Run a crate's Cargo integration suites in one bounded nextest invocation.
[positional-arguments]
suite crate:
    {{ nix }} scripts/test-suite.sh "$1"

# Show a crate's registered integration targets without running tests.
[positional-arguments]
suite-plan crate:
    {{ nix }} scripts/test-suite.sh "$1" --list

# Check that Cargo suites register every integration test file exactly once.
suite-check:
    {{ nix }} scripts/test-suite-check.sh

# Exercise command admission, OOM, cancellation, and descendant cleanup in an
# isolated delegated user service. This does not touch active Exomonad services.
test-command-resources-delegated:
    {{ nix }} exomonad/scripts/test-command-resources-delegated.sh

# Run an inner-loop test selection derived from files changed since BASE.
[positional-arguments]
changed base="HEAD":
    {{ nix }} scripts/test-changed.sh "$1"

# Preview affected targets without compiling or running them.
[positional-arguments]
changed-plan base="HEAD":
    {{ nix }} scripts/test-changed.sh "$1" --list

# Check that committed Haskell CBOR fixtures match current extractor output.
[positional-arguments]
fixtures-check *cohorts:
    {{ nix }} scripts/fixtures.sh check "$@"

# Regenerate the committed Haskell CBOR fixture corpus.
fixtures-update:
    {{ nix }} scripts/fixtures.sh update

# Check that the pure-eval cohort probes still exercise the mechanism their
# cohort claims, per bridge/haskell/test-prepared-stg/probe-opacity-manifest.json.
# Also runs as part of scripts/prepared-corpus.sh (reached by fixtures-check).
probe-opacity-check:
    {{ nix }} scripts/probe-opacity-check.sh

# Inspect the extractor frontend, worker, compiler, and developer tools.
doctor:
    {{ nix }} scripts/toolchain-doctor.sh

# Reuse local Cabal/Cargo outputs to build Exomonad and its matched compiler tools.
exomonad-build:
    {{ exomonad_nix }} bash exomonad/scripts/exomonad-build.sh

# Pass Exomonad init flags after `--`, for example:
#   just exomonad-init -- --session exomonad-tidepool-fresh --no-attach
# Build the matched local tools incrementally, then start an actor run.
[positional-arguments]
exomonad-init *args:
    {{ exomonad_nix }} exomonad/scripts/exomonad-init.sh "$@"

# Build Exomonad from this checkout and run it against the independent console
# repository. Extra arguments are forwarded to `exomonad init`.
[positional-arguments]
exomonad-console *args:
    test -d "$HOME/dev/exomonad-console/.git" || { echo "missing $HOME/dev/exomonad-console; initialize it first" >&2; exit 1; }
    {{ exomonad_nix }} exomonad/scripts/exomonad-init.sh "$@" --workspace "$HOME/dev/exomonad-console"

# Build this Exomonad checkout and launch development in the fresh exomonad-repl project.
[positional-arguments]
exomonad-repl *args:
    test -e "$HOME/dev/exomonad-repl/.git" || { echo "missing $HOME/dev/exomonad-repl; run exomonad new ~/dev/exomonad-repl first" >&2; exit 1; }
    {{ exomonad_nix }} exomonad/scripts/exomonad-init.sh "$@" --workspace "$HOME/dev/exomonad-repl" --model gpt-6-astra --effort medium

# Pre-review gate: check, suite registration, fixtures; all run, all failures reported.
verify:
    {{ nix }} scripts/verify.sh
