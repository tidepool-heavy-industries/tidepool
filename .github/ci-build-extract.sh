#!/usr/bin/env bash
# Build tidepool-extract via nix and print its bin directory.
# The nix flake produces a wrapper with ghcWithPackages (lens, freer-simple).
# On the self-hosted runner, deps are already in the local nix store.
set -euo pipefail

EXTRACT_PATH=$(nix build .#tidepool-extract --no-link --print-out-paths)
echo "$EXTRACT_PATH/bin"
