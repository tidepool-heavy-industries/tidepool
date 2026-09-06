set shell := ["bash", "-euo", "pipefail", "-c"]
nix := if env_var_or_default("IN_NIX_SHELL", "") == "" { "nix develop --command" } else { "" }
shoal_nix := "nix develop .#shoal --command"

# Show the supported development workflow.
default:
    @just --list

# Fast process-isolated workspace unit tests.
quick:
    {{ nix }} scripts/quick.sh

# Format, lint, and run the default-filter test tier in one shell activation.
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

# Run a crate's Cargo integration suites sequentially.
[positional-arguments]
suite crate:
    {{ nix }} scripts/test-suite.sh "$1"

# Show a crate's checked shard plan without running tests.
[positional-arguments]
suite-plan crate:
    {{ nix }} scripts/test-suite.sh "$1" --list

# Check that Cargo suites register every integration test file exactly once.
suite-check:
    {{ nix }} scripts/test-suite-check.sh

# Run an inner-loop test selection derived from files changed since BASE.
[positional-arguments]
changed base="HEAD":
    {{ nix }} scripts/test-changed.sh "$1"

# Check that committed Haskell CBOR fixtures match current extractor output.
fixtures-check:
    {{ nix }} scripts/fixtures.sh check

# Regenerate the committed Haskell CBOR fixture corpus.
fixtures-update:
    {{ nix }} scripts/fixtures.sh update

# Inspect the extractor frontend, worker, compiler, and developer tools.
doctor:
    {{ nix }} scripts/toolchain-doctor.sh

# Build a matched local extractor/worker/Shoal set and start a fresh actor run.
# Pass Shoal init flags after `--`, for example:
#   just shoal-init -- --session shoal-tidepool-fresh --no-attach
[positional-arguments]
shoal-init *args:
    {{ shoal_nix }} scripts/shoal-init.sh "$@"

# Build Shoal from this checkout and run it against the independent console
# repository. Extra arguments are forwarded to `shoal init`.
[positional-arguments]
shoal-console *args:
    test -d "$HOME/dev/shoal-console/.git" || { echo "missing $HOME/dev/shoal-console; initialize it first" >&2; exit 1; }
    {{ shoal_nix }} scripts/shoal-init.sh "$@" --workspace "$HOME/dev/shoal-console"

# Build this Shoal checkout and launch development in the fresh shoal-repl project.
[positional-arguments]
shoal-repl *args:
    test -e "$HOME/dev/shoal-repl/.git" || { echo "missing $HOME/dev/shoal-repl; run shoal new ~/dev/shoal-repl first" >&2; exit 1; }
    {{ shoal_nix }} scripts/shoal-init.sh "$@" --workspace "$HOME/dev/shoal-repl" --model gpt-6-astra --effort medium

# Pre-review gate: checks, suite registration checks, and fixture freshness.
verify: check suite-check fixtures-check
