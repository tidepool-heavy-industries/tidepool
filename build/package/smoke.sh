#!/usr/bin/env bash
set -euo pipefail
bundle="$1"
python="$2"
test -x "$bundle/bin/exomonad"
test -x "$bundle/bin/exomonad-unwrapped"
test -x "$bundle/bin/exomonad-view-helper"
test -x "$bundle/bin/tidepool-extract"
test -x "$bundle/bin/tidepool-extract-bin"
test -s "$bundle/share/exomonad/compiler-deployment.json"
test -s "$bundle/share/exomonad/catalog.json"
test -s "$bundle/share/exomonad/web/index.html"
"$python" - "$bundle" <<'PY'
import json
import sys
from pathlib import Path

bundle = Path(sys.argv[1])
roots = json.loads((bundle / "share/exomonad/runtime-stdlib-deployment.json").read_text())
expected = {
    "bin/tidepool-extract": Path(roots["extract"]) / "bin/tidepool-extract",
    "bin/tidepool-extract-bin": Path(roots["extract"]) / "bin/tidepool-extract-bin",
    "share/exomonad/compiler-deployment.json": Path(roots["extract"]) / "share/exomonad/compiler-deployment.json",
    "share/exomonad/catalog.json": Path(roots["products"]) / "catalog.json",
    "share/exomonad/stdlib": Path(roots["sources"]) / "lib",
}
for name, target in expected.items():
    assert (bundle / name).is_symlink(), name
    assert (bundle / name).readlink() == target, name
assert (bundle / "share/exomonad/ghc-libdir.txt").read_text().strip() == roots["ghc_libdir"]
tools = (bundle / "share/exomonad/runtime-tools").readlink()
assert str(tools).startswith("/nix/store/")
for name in ("bash", "dirname", "git", "bwrap", "tmux", "systemd-run", "systemctl", "nix", "nix-store"):
    assert (tools / "bin" / name).is_file(), name
PY
IFS= read -r revision < "$bundle/share/exomonad/harness-source-revision.txt"
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
