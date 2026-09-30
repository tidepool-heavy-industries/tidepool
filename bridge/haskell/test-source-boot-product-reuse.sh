#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo "usage: $0 TEST_BINARY CACHE_EVEN CACHE_EVEN_BOOT CACHE_ODD CACHE_ENTRY" >&2
  exit 2
fi

test_binary=$1
shift
fixture_dir=test-source-boot/fixtures
mkdir -p "$fixture_dir"
cp -- "$1" "$fixture_dir/CacheEven.hs"
cp -- "$2" "$fixture_dir/CacheEven.hs-boot"
cp -- "$3" "$fixture_dir/CacheOdd.hs"
cp -- "$4" "$fixture_dir/CacheEntry.hs"
"$test_binary"
