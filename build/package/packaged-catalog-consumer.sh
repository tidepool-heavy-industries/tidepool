#!/usr/bin/env bash
set -euo pipefail

descriptor=$1
binary=$2
bubblewrap=$3
runner=$4
python=$5
bundle=$6

mapfile -t roots < <("$python" - "$descriptor" <<'PY'
import json
import sys
from pathlib import Path

descriptor = json.loads(Path(sys.argv[1]).read_text())
for key in ("sources", "products", "extract", "ghc_libdir"):
    root = descriptor[key]
    if not isinstance(root, str) or not root.startswith("/nix/store/") or "\n" in root:
        raise SystemExit(f"invalid immutable deployment root: {key}")
    print(root)
PY
)
if [[ ${#roots[@]} != 4 ]]; then
  echo 'The deployment descriptor must contain four canonical immutable roots.' >&2
  exit 1
fi
sources=${roots[0]}
products=${roots[1]}
extract=${roots[2]}
ghc_libdir=${roots[3]}
test -s "$products/catalog.json"
test -s "$extract/share/exomonad/compiler-deployment.json"
test -x "$extract/bin/tidepool-extract"
test -x "$extract/bin/tidepool-extract-bin"
test -x "$bubblewrap"
runtime_tools="$("$python" -c 'import sys; from pathlib import Path; print(Path(sys.argv[1]).readlink())' "$bundle/share/exomonad/runtime-tools")"
test -x "$runtime_tools/bin/bash"

# Only the immutable closure, retained consumer and fresh process scratch are
# visible. Checkout, Buck and Cargo outputs are absent from this namespace.
exec "$bubblewrap" --unshare-all --die-with-parent --new-session \
  --ro-bind /nix/store /nix/store --proc /proc --dev /dev \
  --tmpfs /tmp --dir /tmp/home --dir /tmp/compile-cache --dir /tmp/build-products \
  --dir /gate --ro-bind "$binary" /gate/consumer \
  --ro-bind "$runner" /gate/isolated-libtest.py \
  --ro-bind "$bundle" /gate/package \
  --clearenv --setenv HOME /tmp/home --setenv TMPDIR /tmp \
  --setenv PATH "$runtime_tools/bin" \
  --setenv TIDEPOOL_EXTRACT "$extract/bin/tidepool-extract" \
  --setenv TIDEPOOL_EXTRACT_WORKER "$extract/bin/tidepool-extract-bin" \
  --setenv TIDEPOOL_COMPILER_DEPLOYMENT "$extract/share/exomonad/compiler-deployment.json" \
  --setenv TIDEPOOL_COMPILER_MODULES "$products/catalog.json" \
  --setenv TIDEPOOL_PRELUDE_DIR "$sources/lib" \
  --setenv TIDEPOOL_GHC_LIBDIR "$ghc_libdir" \
  --setenv TIDEPOOL_EXTRACT_NO_DAEMON 1 \
  --setenv TIDEPOOL_COMPILE_CACHE_DIR /tmp/compile-cache \
  --setenv TIDEPOOL_BUILD_PRODUCTS_DIR /tmp/build-products \
  --chdir /tmp "$runtime_tools/bin/bash" -c '
    set -euo pipefail
    test ! -e /srv/swarm/checkouts
    test ! -e /srv/build
    test -x /gate/package/bin/exomonad-view-helper
    test -s /gate/package/share/exomonad/web/index.html
    /gate/package/bin/exomonad --help >/dev/null
    cat /proc/self/mountinfo
    exec "$1" /gate/isolated-libtest.py /gate/consumer \
      --exact actor_host::packaged_catalog_tests::packaged_cohort_executes_and_displays_without_build_inputs \
      --expected-count 1 --ignored --jobs 1 --timeout 600
  ' packaged-direct-consumer "$python"
