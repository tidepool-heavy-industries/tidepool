#!/usr/bin/env bash
set -euo pipefail
if [[ $# -eq 4 && "$1" == --fresh ]]; then
  exec "$2" "$3" --fresh-test-inputs "$4"
fi
if [[ $# -ne 3 ]]; then
  echo 'usage: corpus-test RUNNER CORPUS EXPECTATIONS' >&2
  exit 2
fi
corpus_runner="$1"
corpus_directory="$2"
corpus_expectations="$3"
corpus_work="$(mktemp -d)"
cleanup() {
  local status=$?
  if [[ "$status" -eq 0 ]]; then
    rm -rf -- "$corpus_work"
  else
    echo "corpus failure evidence: $corpus_work" >&2
  fi
  return "$status"
}
trap cleanup EXIT
"$corpus_runner" verify-cohort "$corpus_directory" "$corpus_expectations" "$corpus_work/results.json"
