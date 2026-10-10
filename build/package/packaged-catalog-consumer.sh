#!/usr/bin/env bash
set -euo pipefail

descriptor=$1
bubblewrap=$2
output=$3
python=$4

# Qualification verifies the original frozen bundle before creating a namespace
# with no checkout, Buck outputs, user cache, or resident compiler endpoint.
exec "$python" - "$descriptor" "$bubblewrap" "$output" "$python" <<'PY'
from pathlib import Path
import runpy
import subprocess
import sys

descriptor, bubblewrap, output, python = map(Path, sys.argv[1:])
owner = descriptor.parent / "qualification.py"
qualification = runpy.run_path(str(owner))
record = qualification["verify"](descriptor)
bundle = Path(record["bundle_root"])
if (record["stdlib_mode"] != "catalog-backed"
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
# The verified bundle's qualification owner selects the runner resources for
# both cohorts and this namespace. The consumer always requires its catalog.
command.extend(qualification["NativeRunnerResources"].from_environment(
    record["environment"], require_catalog=True).arguments())
raise SystemExit(subprocess.run(command, check=False).returncode)
PY
