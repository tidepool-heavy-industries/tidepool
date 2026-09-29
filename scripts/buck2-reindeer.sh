#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
mode="${1:-}"
if [ -n "$mode" ] && [ "$mode" != "--check" ]; then
  echo "usage: $0 [--check]" >&2
  exit 2
fi
original=""
if [ "$mode" = "--check" ]; then
  original="$(mktemp)"
  if [ -f third-party/rust/BUCK ]; then
    cp third-party/rust/BUCK "$original"
  else
    : > "$original"
  fi
  restore_graph() {
    if [ -s "$original" ]; then
      cp "$original" third-party/rust/BUCK
    else
      rm -f third-party/rust/BUCK
    fi
    rm -f "$original"
  }
  trap restore_graph EXIT
fi

if [ -z "${REINDEER_BIN:-}" ]; then
  reindeer_store="$(nix build --no-link --print-out-paths .#buck-reindeer)"
  REINDEER_BIN="$reindeer_store/bin/reindeer"
fi
"$REINDEER_BIN" -c third-party/rust/reindeer.toml buckify
python3 - <<'PY'
from collections import Counter
from pathlib import Path
import re

path = Path("third-party/rust/BUCK")
source = path.read_text()
pattern = re.compile(
    r'^alias\(\n    name = "([^"]+)",\n    actual = ":[^"]+",\n'
    r'    visibility = \["PUBLIC"\],\n\)\n\n',
    re.MULTILINE,
)
aliases = Counter(match.group(1) for match in pattern.finditer(source))
duplicates = {name for name, count in aliases.items() if count > 1}
if duplicates != {"sha2"}:
    raise SystemExit(f"unexpected Reindeer unversioned alias collisions: {sorted(duplicates)}")
path.write_text(pattern.sub(lambda match: "" if match.group(1) in duplicates else match.group(0), source))
source = path.read_text()
source, count = re.subn(
    r'(cargo\.rust_library\(\n    name = "sha2-0\.(?:10|11)",.*?    visibility = )\[\]',
    r'\1["PUBLIC"]',
    source,
    flags=re.DOTALL,
)
if count != 2:
    raise SystemExit(f"expected two public sha2 versioned targets, found {count}")
path.write_text(source)
PY
if [ "$mode" = "--check" ]; then
  if ! cmp -s "$original" third-party/rust/BUCK; then
    echo "third-party/rust/BUCK is stale; run scripts/buck2-reindeer.sh to regenerate from Cargo.lock" >&2
    diff -u "$original" third-party/rust/BUCK || true
    exit 1
  fi
fi
