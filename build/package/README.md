`//build/package:native_runtime_bundle` carries Buck's host, libtest, extractor
frontend and worker, declared shared libraries, Haskell sources, actors and
browser assets. With no native catalog selection it uses `source-backed` mode.
An explicit retained source root or retention record selects `catalog-backed`
mode and requires the complete valid selection; invalid configuration cannot
fall back to source compilation. `native_source_runtime_bundle` remains the
explicit source-backed development target. The older Nix `matched_runtime_bundle`
is a separate package owner until the native catalog delivery gate passes.

`//build/package:native_catalog_sources` snapshots the existing runtime `lib`
and `actors` trees, the genuine generated stable effect modules, and pinned Jev
`core` sources. Its import probe is projected from source-owned modules across
the pinned Cabal component metadata and the existing stable-effect/Jev source
owners. Test-only source roots, test package dependencies, workspace-specific
Orchestrate modules and the invocation-specific `Tidepool.Effects` shim are
excluded. Production catalog admission still checks the actual compiler's
complete closure; direct import selection alone is not passing evidence.

The current metadata projects 67 direct imports. These are source selections;
compiler-produced module counts require the native action and gate report.

| Evidence | Owner | Current count |
| --- | --- | ---: |
| Stable effect direct imports | Generated effect roster | 3 |
| Stdlib direct imports | Pinned Cabal source ownership | 53 |
| Actor direct imports | Pinned Cabal source ownership | 7 |
| Jev direct imports | Pinned Jev source projection | 4 |
| Full source file inventory | Retained snapshot record | Requires retention run |
| Compiled module closure | Native catalog producer/admission | Requires native build |
| Cold consumer execution | Frozen `catalog-gate` report | Requires gate run |

Retain that exact snapshot before compiling original module products:

```sh
CATALOG_SOURCES="$(python3 build/package/qualification.py retain-sources \
  --snapshot "$BUCK_CATALOG_SOURCES" --output "$SOURCE_RETENTION" \
  --runtime-tools "$DECLARED_RUNTIME_TOOLS")"
CATALOG_RECORD="$SOURCE_RETENTION/share/exomonad/retained-catalog-sources.json"
```

The qualification owner compares full file inventories, verifies the original
Nix path and NAR identity, and creates and checks a registered GC root.
`select-sources --record RECORD --snapshot SNAPSHOT --runtime-tools TOOLS`
revalidates that exact retention when resuming. Source names and bytes determine
the original store root. Retain the record and collector root until freezing
transfers retention to the final bundle.

The native action consumes the current source snapshot, a declared copy of the
original retained root, and declared retention-record bytes. Stage the exact
selection through `scripts/buck2-configure.sh` or root Buck configuration:
`nix.native_catalog_source_root` is the returned original root;
`nix.native_catalog_retention_record` is the canonical record path;
`nix.native_catalog_retention` is that record's exact JSON value. The declared
record artifact is generated from configuration bytes; the mutable evidence
path supplies only the original collector-root location.

`//build/package:native_catalog` rechecks all three source inventories and actual
Nix retention before invoking the existing Buck `tidepool-module-package` with
the original `TidepoolCatalog.hs`, `catalogSentinel`, original source root and
ordinary action output directory. Frontend, worker, compiler deployment, GHC
libdir and runtime libraries are explicit action inputs. It rechecks source
retention after production and binds the complete product inventory, catalog
SHA-256 and schema-4 source selection. The roles are ordered as stable effects,
stdlib, actors and Jev, under one original root. Producer/worker identities,
source evidence and product metadata are copied without rewriting.

The catalog-backed bundle retains those exact products and selection, with no
unused stdlib or actor source copies. Freezing
validates their inventories and NAR/collector evidence, creates a registered
source GC root owned by the final bundle, and seals a new retention record.
The original retention may then be retired independently. Qualified execution
and the native entrypoint select the frozen catalog with its original `lib`
and actor roots. Rust catalog admission owns the BLAKE3 source manifest and
module/interface validation; qualification also binds the full original source
inventory and NAR identity. Source tests do not establish a compiled native
catalog, M1/M2 execution or live deployment acceptance.

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
Configuration retains the original Git objects for that recorded commit in an
immutable Git bundle. Native scaffold tests and frozen launches receive the
declared bundle through `EXOMONAD_WORKSPACE_GIT_BUNDLE`; qualification checks its
commit and object closure against the Gitlink and seals the copied bundle bytes.
Scaffolding clones those objects locally and records the public upstream URL for
future submodule updates. Test execution requires no mutable source repository.

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
parent survival, later nominal publication join, checkpoint release and the selected
coding child. The selected child case scaffolds the shipped workspace and passes a
`Project.Work` Task from the root to a coding child through compiler admission,
then checks the child's narrower effect row and original typed reply. Checkpoint
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

The catalog acceptance route requires the frozen descriptor. It verifies the
exact native selection before entering the consumer namespace, exposes the
original frozen bundle at its canonical path, and runs the one mandatory
catalog consumer in fresh caches with checkout and Buck build inputs absent.
The report requires one executed passing test; compilation or an empty
selection cannot satisfy it.

```sh
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" catalog-gate \
  "$DESCRIPTOR" --output "$CATALOG_GATE_EVIDENCE"
```

The native `bin/exomonad` entrypoint independently verifies the retained
qualification descriptor and obtains its environment from that owner before
executing the host. An assembled action output must therefore be frozen before
launching it directly.

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
