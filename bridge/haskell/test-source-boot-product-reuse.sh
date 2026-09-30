#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: $0 TEST_BINARY CACHE_EVEN CACHE_EVEN_BOOT CACHE_ODD CACHE_ENTRY" >&2
  exit 2
fi

test_binary=$(realpath -- "$1")
shift
fixture_sources=()
for source in "$@"; do
  fixture_sources+=("$(realpath -- "$source")")
done
test_root=$(mktemp -d "${TMPDIR:-/tmp}/tidepool-source-boot.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
cd "$test_root"
fixture_dir=test-source-boot/fixtures
mkdir -p "$fixture_dir"
cp -- "${fixture_sources[0]}" "$fixture_dir/CacheEven.hs"
cp -- "${fixture_sources[1]}" "$fixture_dir/CacheEven.hs-boot"
cp -- "${fixture_sources[2]}" "$fixture_dir/CacheOdd.hs"
cp -- "${fixture_sources[3]}" "$fixture_dir/CacheEntry.hs"
"$test_binary"
