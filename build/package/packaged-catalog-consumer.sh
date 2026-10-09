#!/usr/bin/env bash
set -euo pipefail

bundle=$1
descriptor=$2
bubblewrap=$3
output=$4
python=$5

# Qualification verifies the original frozen bundle before creating a namespace
# with no checkout, Buck outputs, user cache, or resident compiler endpoint.
exec "$python" - "$bundle" "$descriptor" "$bubblewrap" "$output" "$python" <<'PY'
import json
from pathlib import Path
import subprocess
import sys

bundle, descriptor, bubblewrap, output, python = map(Path, sys.argv[1:])
record = json.loads(descriptor.read_text())
if (record["bundle_root"] != str(bundle) or record["stdlib_mode"] != "catalog-backed"
        or record["programs"]["libtest"] != str(bundle / "bin/tidepool-tests")):
    raise SystemExit("catalog gate requires the exact qualified native bundle")
environment = dict(record["environment"])
environment.update({
    "HOME": "/tmp/home", "TMPDIR": "/tmp", "XDG_CACHE_HOME": "/tmp/cache",
    "TIDEPOOL_EXTRACT_NO_DAEMON": "1", "TIDEPOOL_COMPILE_CACHE_DIR": "/tmp/compile-cache",
    "TIDEPOOL_BUILD_PRODUCTS_DIR": "/tmp/build-products",
})
for key in ("TIDEPOOL_EXTRACT_DAEMON_SOCKET",):
    environment.pop(key, None)
command = [str(bubblewrap), "--unshare-all", "--die-with-parent", "--new-session",
           "--ro-bind", "/nix/store", "/nix/store", "--proc", "/proc", "--dev", "/dev",
           "--tmpfs", "/tmp", "--dir", "/tmp/home", "--dir", "/tmp/cache",
           "--dir", "/tmp/compile-cache", "--dir", "/tmp/build-products",
           "--dir", str(bundle.parent), "--ro-bind", str(bundle), str(bundle),
           "--bind", str(output), "/evidence", "--clearenv"]
for name, value in sorted(environment.items()):
    command.extend(["--setenv", name, value])
command.extend(["--chdir", "/tmp", str(python), record["programs"]["runner"],
                record["programs"]["libtest"],
                "--exact", "actor_host::packaged_catalog_tests::packaged_cohort_executes_and_displays_without_build_inputs",
                "--expected-count", "1", "--ignored", "--jobs", "1", "--timeout", "900",
                "--output-dir", "/evidence/tests"])
# The isolated runner clears undeclared compiler selections. Carry the catalog
# required by this consumer and the descriptor's optional native test resources.
command.extend(["--resource-env", "TIDEPOOL_COMPILER_MODULES"])
for name in ("TIDEPOOL_PREPARED_ROOT_ENTRY", "TIDEPOOL_PREPARED_BUILTIN_ENTRIES", "TIDEPOOL_TEST_FIXTURE_ROOT"):
    if name in record["environment"]:
        command.extend(["--resource-env", name])
raise SystemExit(subprocess.run(command, check=False).returncode)
PY
