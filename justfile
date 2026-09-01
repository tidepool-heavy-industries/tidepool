set shell := ["bash", "-euo", "pipefail", "-c"]
nix := if env_var_or_default("IN_NIX_SHELL", "") == "" { "nix develop --command" } else { "" }

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

# Run every declared shard for a large GHC-heavy crate, sequentially.
[positional-arguments]
suite crate:
    {{ nix }} scripts/test-suite.sh "$1"

# Show a crate's checked shard plan without running tests.
[positional-arguments]
suite-plan crate:
    {{ nix }} scripts/test-suite.sh "$1" --list

# Validate that the suite manifest names every integration-test binary once.
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
    {{ nix }} scripts/shoal-init.sh "$@"

# Pre-review gate: checks, suite-manifest validation, and fixture freshness.
verify: check suite-check fixtures-check
