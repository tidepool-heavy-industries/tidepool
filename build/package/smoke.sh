#!/usr/bin/env bash
set -euo pipefail
bundle="$1"
test -x "$bundle/bin/exomonad"
test -x "$bundle/bin/tidepool-extract"
test -x "$bundle/bin/tidepool-extract-bin"
test -s "$bundle/share/exomonad/web/index.html"
IFS= read -r revision < "$bundle/share/exomonad/harness-source-revision.txt"
IFS= read -r ghc_libdir < "$bundle/share/exomonad/ghc-libdir.txt"
test "$ghc_libdir" = "$TIDEPOOL_GHC_LIBDIR"
IFS= read -r ghc_version < "$bundle/share/exomonad/ghc-version.txt"
[[ "$ghc_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
[[ "$revision" =~ ^[0-9a-f]{40}$ ]]
help="$("$bundle/bin/exomonad" --help)"
for command in new init check run-map; do
  [[ "$help" == *"$command"* ]]
done
worker_flag="$("$bundle/bin/tidepool-extract-bin" --print-worker-request-flag)"
[[ "$worker_flag" =~ ^--worker-request-v[0-9]+$ ]]
set +e
extract_error="$("$bundle/bin/tidepool-extract" 2>&1)"
status=$?
set -e
test "$status" -eq 2
[[ "$extract_error" == *"Usage: tidepool-extract"* ]]
