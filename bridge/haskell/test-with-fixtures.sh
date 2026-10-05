#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 TEST_BINARY FIXTURE_TREE [TASTY_ARGUMENTS...]" >&2
  exit 2
fi

test_binary=$(realpath -- "$1")
fixture_tree=$(realpath -- "$2")
shift 2
# RunInfo may supply paths relative to the launch directory; TestInfo supplies
# absolute paths. Input roles come from the same native component declarations.
launch_directory=$PWD
normalize_search_list() {
  local remaining=$1 role=$2 entry rooted result="" separator="" more
  while true; do
    if [[ "$remaining" == *:* ]]; then
      entry=${remaining%%:*}
      remaining=${remaining#*:}
      more=true
    else
      entry=$remaining
      more=false
    fi
    # Dynamic loader tokens belong to the loaded artifact rather than CWD.
    if [[ "$entry" == /* ]] || [[ "$role" == library-search-list && (
      "$entry" == *'$ORIGIN'* || "$entry" == *'${ORIGIN}'*
      || "$entry" == *'$LIB'* || "$entry" == *'${LIB}'*
      || "$entry" == *'$PLATFORM'* || "$entry" == *'${PLATFORM}'*) ]]; then
      rooted=$entry
    else
      # Empty search entries select the launch CWD, including a trailing colon.
      rooted=$launch_directory/${entry:-.}
    fi
    result+=$separator$rooted
    separator=:
    [[ "$more" == true ]] || break
  done
  printf '%s' "$result"
}
compiler_deployment_declared=false
read -ra path_inputs <<< "${TIDEPOOL_TEST_INPUT_PATHS:?missing declared native test input roles}"
for input in "${path_inputs[@]}"; do
  name=${input%%:*}
  role=${input#*:}
  if [[ ! "$name" =~ ^[A-Z_][A-Z_0-9]*$ || ! -v "$name" ]]; then
    echo "missing or invalid declared native test input: $name" >&2
    exit 2
  fi
  if [[ "$name" == TIDEPOOL_COMPILER_DEPLOYMENT && "$role" != file ]]; then
    echo "compiler deployment requires its declared file role" >&2
    exit 2
  fi
  value=${!name}
  case "$role" in
    search-list|library-search-list) rooted=$(normalize_search_list "$value" "$role") ;;
    file|directory|executable)
      rooted=$(realpath -e -- "$value")
      case "$role" in
        file) [[ -f "$rooted" ]] ;;
        directory) [[ -d "$rooted" ]] ;;
        executable) [[ -f "$rooted" && -x "$rooted" ]] ;;
      esac || { echo "invalid declared $role input: $name" >&2; exit 2; }
      ;;
    *) echo "invalid declared native test input role: $role" >&2; exit 2 ;;
  esac
  export "$name=$rooted"
  if [[ "$name" == TIDEPOOL_COMPILER_DEPLOYMENT ]]; then
    compiler_deployment_declared=true
  fi
done
# Read configured authority; the Rust issuer still independently admits the
# exact frontend/worker deployment before issuing any certificate.
# The structural codec uses the same libtest executable without compiler
# authority. Only the validated deployment resource selects genuine issuance.
unset TIDEPOOL_COMPILER_PRODUCER
if [[ "$compiler_deployment_declared" == true ]]; then
  export TIDEPOOL_COMPILER_PRODUCER
  TIDEPOOL_COMPILER_PRODUCER=$("$TIDEPOOL_TEST_PYTHON" - "$TIDEPOOL_COMPILER_DEPLOYMENT" <<'PYDEPLOY'
import json
import sys
from pathlib import Path
path = Path(sys.argv[1])
if path.stat().st_size > 4 << 20:
    raise SystemExit("deployment manifest exceeds metadata bound")
manifest = json.loads(path.read_text())
identity = manifest["producer_identity"]
if manifest["schema"] != 1 or len(identity) != 32 or any(type(value) is not int or not 0 <= value <= 255 for value in identity):
    raise SystemExit("invalid declared compiler producer identity")
print(bytes(identity).hex())
PYDEPLOY
  )
fi
test_root=$(mktemp -d "${TMPDIR:-/tmp}/tidepool-haskell-fixtures.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
# Copy declared inputs so no test can write into Buck's source symlink tree.
cp -RL -- "$fixture_tree/." "$test_root/"
cd "$test_root"
"$test_binary" "$@"
