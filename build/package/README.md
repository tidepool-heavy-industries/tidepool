The first native delivery uses `//build/package:native_runtime_bundle`. It carries
Buck's host, libtest, extractor frontend and worker, declared shared libraries, Haskell
sources, actors and browser assets. Its `source-backed` stdlib mode compiles
through the normal compiler; it does not qualify a canonical module catalog or
durable module-cache deployment. The catalog-backed `matched_runtime_bundle`
is a separate package mode and cannot substitute for this qualification.

The native catalog migration starts with `//build/package:native_catalog_sources`.
It snapshots the existing runtime `lib` and `actors` trees and the genuine generated
Core/Authored pair. Its import probe is projected from the pinned Cabal
`native-helper-contract` module metadata, excluding test source owners and adding
Authored from the existing effect-module roster. Test dependencies and the
invocation-specific `Tidepool.Effects` shim are not catalog inputs. The actual
compiler's complete module closure must still pass catalog admission; this
initial selection does not cover every shipped helper.

Retain that exact source snapshot before compiling original module products:

```sh
CATALOG_SOURCES="$(python3 build/package/qualification.py retain-sources \
  --snapshot "$BUCK_CATALOG_SOURCES" --output "$SOURCE_RETENTION" \
  --runtime-tools "$DECLARED_RUNTIME_TOOLS")"
```

The same qualification owner checks the source inventory, Nix registration and
NAR identity, and retains a GC root. The command returns the original canonical
source root directly for the subsequent native action; it does not reconfigure
the toolchain. `select-sources --record RECORD --snapshot SNAPSHOT --runtime-tools TOOLS`
revalidates a previously retained selection when resuming delivery. Fixed source
names and bytes give the same retained original paths
across runs. The source retention record must survive until final qualification
retains those roots. This preparation boundary alone does not establish a native
catalog deployment; the schema, packaging and runtime selection cutover remain
required before catalog-backed qualification.

Build the native bundle and
`//build/testing/browser:driver_bundle` in one selected native profile. Retain the
actual successful build log and its argv arrays as JSON. Source must have clean
tracked/index state and initialized clean submodules at their recorded commits;
untracked source handoffs and retired directories are preserved.

Freeze to a new, canonical final path. This copies Buck symlinks into regular
artifact bytes, generates the compiler deployment manifest there, records exact
source/Harness/profile provenance, hashes copied artifact bytes, records Nix NAR
identities, and checks the actual ELF loader dependencies. Compiler or
library paths into mutable checkout/Buck trees are rejected. Existing resident
daemons and project catalogs cannot override this deployment.

The bundle action owns its libtest and executable inputs. Its build contract
records their hashes and the declared native source snapshot under one selected
profile. Freezing compares that snapshot with the clean recorded Git source and
rejects a different requested profile, source revision, or substituted binary.
The source OID and build log add provenance; they do not replace those byte checks.
The bundled workspace Gitlink is also checked against the source HEAD's recorded
`.exomonad/workspace` submodule. Hosted tests and the actual binary receive that
same frozen file through `EXOMONAD_WORKSPACE_GITLINK`.

```sh
python3 build/package/qualification.py freeze \
  --bundle "$BUCK_NATIVE_BUNDLE" --output "$FINAL_BUNDLE" \
  --source-root "$SOURCE_ROOT" --expect-profile fast-dev \
  --browser-driver "$BUCK_BROWSER_DRIVER_BUNDLE" \
  --browser-node "$DECLARED_BROWSER_NODE" \
  --playwright-browsers "$DECLARED_PLAYWRIGHT_BROWSERS" \
  --build-log "$SUCCESSFUL_BUILD_LOG" --build-commands "$BUILD_ARGV_JSON"
```

Use the one resulting descriptor for both cohorts. The existing isolated libtest
runner executes each test in a fresh bounded process. Reports retain exact
names, actual counts, exit status, elapsed time and bounded stdout/stderr. An
unknown executed count or zero selection cannot qualify a passing cohort.
`run --jobs N` selects bounded concurrency (default one); `--delegated-service`
and `--service-slice NAME.slice` use the runner's fresh delegated user services
inside an already admitted user slice. Reports retain these scheduling choices.
The descriptor seals M2 watchdogs at 600 seconds, with 900 seconds for unfinished
parent survival, later nominal publication join and checkpoint release. Checkpoint
release retains the issuer settlement, observer creation, original-scope read/reply
and final cleanup in one watchdog; the measured issuer portion already took
538 seconds before those later phases. These outer process limits
preserve the tests' internal phase and cancellation assertions.

```sh
DESCRIPTOR="$FINAL_BUNDLE/share/exomonad/qualification.json"
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort m2 --output "$M2_EVIDENCE" --jobs 3 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort m1 --output "$M1_EVIDENCE"
```

For the parallel command, `ADMITTED_USER_SLICE` names an existing user slice
whose resource bounds have been checked for the chosen concurrency.

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
