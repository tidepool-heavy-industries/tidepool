The first native delivery uses `//build/package:native_runtime_bundle`. It carries
Buck's host, extractor frontend and worker, declared shared libraries, Haskell
sources, actors and browser assets. Its `source-backed` stdlib mode compiles
through the normal compiler; it does not qualify a canonical module catalog or
durable module-cache deployment. The catalog-backed `matched_runtime_bundle`
is a separate package mode and cannot substitute for this qualification.

Build the native bundle, `//bridge/facade:tidepool_unit_tests`, and
`//build/testing/browser:driver_bundle` in one selected native profile. Retain the
actual successful build log and its argv arrays as JSON. Source must have clean
tracked/index state and initialized clean submodules at their recorded commits;
untracked source handoffs and retired directories are preserved.

Freeze to a new, canonical final path. This copies Buck symlinks into regular
artifact bytes, generates the compiler deployment manifest there, records exact
source/Harness/profile provenance, hashes the runtime inventory and declared Nix
toolchain inputs, and checks the actual ELF loader dependencies. Compiler or
library paths into mutable checkout/Buck trees are rejected. Existing resident
daemons and project catalogs cannot override this deployment.

```sh
python3 build/package/qualification.py freeze \
  --bundle "$BUCK_NATIVE_BUNDLE" --output "$FINAL_BUNDLE" \
  --source-root "$SOURCE_ROOT" --libtest "$BUCK_FACADE_LIBTEST" \
  --browser-driver "$BUCK_BROWSER_DRIVER_BUNDLE" \
  --browser-node "$DECLARED_BROWSER_NODE" \
  --playwright-browsers "$DECLARED_PLAYWRIGHT_BROWSERS" \
  --build-log "$SUCCESSFUL_BUILD_LOG" --build-commands "$BUILD_ARGV_JSON"
```

Use the one resulting descriptor for both cohorts. The existing isolated libtest
runner executes each test in a fresh bounded process. Reports retain exact
names, actual counts, exit status, elapsed time and bounded stdout/stderr. An
unknown executed count or zero selection cannot qualify a passing cohort.

```sh
DESCRIPTOR="$FINAL_BUNDLE/share/exomonad/qualification.json"
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort m2 --output "$M2_EVIDENCE"
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort m1 --output "$M1_EVIDENCE"
```

Launch the same package bytes for the actual recursive live smoke. `exec`
verifies the descriptor and runtime dependencies, supplies the same environment,
and records process execution. The owning live scenario separately retains real
actor descendants, provider replies, browser/input/interrupt behavior and cleanup;
a process exit alone does not establish those outcomes. Help/version requests
are rejected by this entrypoint.

```sh
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" exec \
  --report "$LIVE_PROCESS_REPORT" "$DESCRIPTOR" -- \
  init --workspace "$LIVE_WORKSPACE" --session "$LIVE_SESSION" --no-attach
```
