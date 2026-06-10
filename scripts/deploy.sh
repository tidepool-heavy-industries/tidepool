#!/usr/bin/env bash
set -euo pipefail

# Tidepool Toolchain Deployment Script
# Reinstalls MCP server, rebuilds Haskell extract binary, and clears cache.

SKIP_HASKELL=false
if [[ "${1:-}" == "--skip-haskell" ]]; then
  SKIP_HASKELL=true
fi

echo "(1/4) Installing Tidepool MCP server..."
cargo install --path tidepool

if [ "$SKIP_HASKELL" = false ]; then
  echo "(2/4) Rebuilding and installing Haskell toolchain (via nix develop)..."
  nix develop -c bash -c 'cd haskell && cabal build tidepool-extract-bin'
  cp "$(nix develop -c bash -c 'cd haskell && cabal list-bin tidepool-extract-bin')" ~/.local/bin/tidepool-extract-bin
else
  echo "(2/4) Skipping Haskell toolchain rebuild (--skip-haskell)."
fi

echo "(3/4) Clearing compile cache..."
rm -rf ~/.cache/tidepool/

echo "Deployed. Reconnect the tidepool MCP server (/mcp) to pick up changes."
