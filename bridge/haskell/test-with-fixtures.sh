#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 TEST_BINARY FIXTURE_TREE" >&2
  exit 2
fi

test_binary=$(realpath -- "$1")
fixture_tree=$(realpath -- "$2")
test_root=$(mktemp -d "${TMPDIR:-/tmp}/tidepool-haskell-fixtures.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
# Copy declared inputs so no test can write into Buck's source symlink tree.
cp -RL -- "$fixture_tree/." "$test_root/"
cd "$test_root"
"$test_binary"
