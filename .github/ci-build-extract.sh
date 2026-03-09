#!/usr/bin/env bash
# Build tidepool-extract with persistent cabal cache and emit a wrapper script.
# Usage: source <(./ci-build-extract.sh)
#   This prints PATH=... to stdout; sourcing it adds tidepool-extract to PATH.
set -euo pipefail

CABAL_STORE="${CABAL_STORE:-/var/lib/github-runner-cache/cabal/store}"
CABAL_BUILDDIR="${CABAL_BUILDDIR:-/var/lib/github-runner-cache/cabal/dist-newstyle}"

cd "$(dirname "$0")/../haskell"

cabal update
cabal build exe:tidepool-extract-bin \
  --store-dir="$CABAL_STORE" \
  --builddir="$CABAL_BUILDDIR"

BIN=$(cabal list-bin tidepool-extract-bin \
  --store-dir="$CABAL_STORE" \
  --builddir="$CABAL_BUILDDIR")

GHC_DIR=$(dirname "$(which ghc)")

WRAPPER_DIR=$(mktemp -d)
cat > "$WRAPPER_DIR/tidepool-extract" <<EOF
#!/usr/bin/env bash
export PATH="$GHC_DIR:\$PATH"
exec "$BIN" "\$@"
EOF
chmod +x "$WRAPPER_DIR/tidepool-extract"

echo "$WRAPPER_DIR"
