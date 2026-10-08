`//build/package:native_runtime_bundle` is the prepared release target. It
carries Buck's host, libtest, extractor frontend and worker, declared shared
libraries, browser assets, the authenticated native catalog and a complete
original root entry. It requires the retained source and compiler selection.

`//build/package:native_catalog_runtime_bundle` selects the same prepared
assembly. `//build/package:source_backed_developer_bundle` is an unprepared
developer path and does not establish prepared startup acceptance. Build, freeze
and consumer qualification of one exact prepared bundle remain required before
delivery.

Both native bundles require executable `bash`, `python3`, `dirname`, `git`,
`bwrap`, `tmux`, `systemd-run`, `systemctl`, `nix` and `nix-store` in their declared
runtime-tools closure. Qualification checks these inputs during assembly,
freezing and runtime environment selection. It cannot substitute ambient Python
or other executables for missing package inputs.

`//build/package:native_catalog_sources` snapshots the existing runtime `lib`
and `actors` trees, the genuine generated stable effect modules, and pinned Jev
`core` sources. Its import probe is projected from source-owned modules across
the pinned Cabal component metadata and the existing stable-effect/Jev source
owners. Test-only source roots, test package dependencies, workspace-specific
Orchestrate modules are excluded. `Tidepool.Effects` is a stable authored
facade and belongs to the generated support selection. Production catalog admission still checks the actual compiler's
complete closure; direct import selection alone is not passing evidence.

The snapshot also includes `TidepoolPreparedDriver.hs` beside the catalog probe,
outside the library roots, produced from
the runtime's existing settled-entry renderer. Its fixed entry and effect row
name `Tidepool.Actors.Internal.ExomonadDriver.rootDriver` and `RootEffects`.
That source joins the same complete source inventory and Nix retention as the
catalog; no build scratch path becomes a runtime source witness.

`NATIVE_CATALOG_COHORT` in the generated `bridge/haskell/components.bzl` owns
the direct import roster. The snapshot's `catalog-sources.json` preserves that
exact module-to-source selection; its `modules` entries determine direct import
counts, including each source role. Compiler-produced counts require genuine
certification and do not follow from this roster.

| Evidence | Owner | Recorded by |
| --- | --- | --- |
| Direct imports and source roles | Pinned Cabal and stable-effect/Jev source projection | Snapshot `catalog-sources.json` |
| Full source file inventory | Source retention and qualification | Retained snapshot record |
| Canonical interfaces and optional Core | Actual compiler product certification | `inspect-catalog` inventory |
| Native owners and groups | Actual compiler product certification | `inspect-catalog` inventory |
| Cold consumer execution | Frozen native catalog bundle | `catalog-gate` report |

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
selection through root Buck configuration:
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

`//build/package:native_root_entry` uses that same retained source and declared
compiler boundary. Its separate production entry container preserves the full
original metadata, target program, yield sites, certified groups, package and
import owners, canonical interfaces and native products. The runtime loader
validates this complete original container against the configured deployment
and source selection through the existing original-output owner. Portable
prepared fixtures carry no production authority. Loading the root entry does
not execute source or require a live compiler.

Production entries use schema 2, with an explicit native-catalog or frozen
workspace source selection. Schema 1 entries must be rebuilt. Publication syncs
the complete staged tree before rename and the established output parent after
rename. `EntryPublicationUnconfirmed` names an already visible output: validate
and load that original, then retry parent durability confirmation without
executing source again.

The prepared bundle's qualification contract retains the root entry inventory
and selects `TIDEPOOL_PREPARED_ROOT_ENTRY`; root and child machines install fresh
mutable state from the retained decoded entry and native images. General
workspace entry export uses the same container with a typed ordered frozen
source selection. Its source owner must retain the original generated wrapper
and every selected root. The runtime compiler endpoint can then publish a
complete entry, including completed quotations, without making source replay
eligible. The native producer retains its declared Nix source contract; it does
not accept transient workspace source directories. Durable workspace selection
and CLI preparation remain separate consumer obligations.

The catalog-backed bundle retains those exact products and selection, with no
unused stdlib or actor source copies. Freezing
validates their inventories and NAR/collector evidence, creates a registered
source GC root owned by the final bundle, and seals a new retention record.
The original retention may then be retired independently. Qualified execution
and the native entrypoint select the frozen catalog with its original `lib`
and actor roots. Rust catalog admission owns the SHA-256 source manifest and
module/interface validation; qualification also binds the full original source
inventory and NAR identity. Source tests do not establish a compiled native
catalog, M1/M2 execution or live deployment acceptance.

Schema 4 source witnesses are ordered `{path, sha256}` records for `.hs`,
`.hs-boot`, `.lhs` and `.lhs-boot` files, including the import probe. The
unpublished tuple witness format is rejected; regenerate catalogs through the
matched native producer. Generic source and cache identities retain BLAKE3.

When catalog production refuses admission, inspect the same declared inputs
through the existing owner. `inspect-catalog` invokes the producer's typed
`inspect` operation with the same original probe, ordered source roots and
explicit compiler deployment. It never creates a catalog build receipt or
qualification acceptance. The producer reports inventory only after its real
product-certification boundary; unsafe or incomplete source evidence remains
a refusal with retained raw diagnostics.

```sh
swarm-build "$DECLARED_RUNTIME_TOOLS/bin/python3" build/package/qualification.py inspect-catalog \
  --snapshot "$BUCK_CATALOG_SOURCES" \
  --source-root "$CATALOG_SOURCES" --declared-source-root "$BUCK_DECLARED_SOURCE_ROOT" \
  --retention-record "$BUCK_DECLARED_RETENTION_RECORD" --retention-record-origin "$CATALOG_RECORD" \
  --runtime-tools "$DECLARED_RUNTIME_TOOLS" --producer "$BUCK_MODULE_PACKAGE" \
  --frontend "$BUCK_FRONTEND" --worker "$BUCK_WORKER" \
  --deployment "$BUCK_COMPILER_DEPLOYMENT" --ghc-libdir "$DECLARED_GHC_LIBDIR" \
  --libraries "$BUCK_WORKER_RUNTIME_LIBRARIES" \
  --output "$INSPECTION_OUTPUT" --timeout 900
```

All `BUCK_` paths above are the exact declared outputs used by the catalog action,
not ambient compiler selections. `INSPECTION_OUTPUT` is a new canonical path in
private evidence. The sibling `INSPECTION_OUTPUT.invocation` holds the exact
argv and declared environment, stdout, stderr and final outcome, and is the producer's working
directory so retained outer scratch stays with its evidence. The producer owns
recursive compiler diagnostics under its output. Inspection requires an explicit
producer wall limit of 600–1800 seconds. Python kills and waits for the producer
on timeout; extractor launches, including the startup protocol probe in
`PreparedWorker::check_request_protocol`, use the process owner's parent-death
contract. `protocol_probe_uses_the_owned_parent_death_contract` checks that
probe's signal, and `configured_child_dies_when_its_parent_exits` checks that
an owned child exits when its parent exits. These source tests do not
qualify cleanup of the full inspection descendant tree after a producer timeout;
retain timeout-run evidence and confirm that descendants are gone. Nix preflight
and final retention checks have their existing separate limits. Complete
inventories, NAR identity and actual GC registration are rechecked after success,
refusal or timeout. A producer refusal and a later retention failure are
recorded separately.

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

Use the one resulting descriptor for all cohorts. The existing isolated libtest
runner executes each test in a fresh bounded process. Reports retain exact
names, actual counts, exit status, elapsed time and bounded stdout/stderr. An
unknown executed count or zero selection cannot qualify a passing cohort.
`run --jobs N` selects bounded concurrency (default one); `--delegated-service`
and `--service-slice NAME.slice` use the runner's fresh delegated user services
inside an already admitted user slice. Reports retain these scheduling choices.
The frozen descriptor is the authority for the exact M1/M2 case rosters, counts
and deadlines. M2 has a 600-second default watchdog; unfinished-parent survival,
nominal publication join, checkpoint release and
`actor_host::fresh_child_tests::scaffolded_selected_coding_child_preserves_workspace_input_and_effect_row`
each have 900 seconds. This case scaffolds the shipped workspace and passes a
`Project.Work` Task from the root to a spawned child through compiler admission
with the explicitly supplied `defaultWorkbenchSpec` effect row
`[Replies, Commands, Lookup, BoundWorktree]`; it verifies that exact row, rejects
a Journal-using notebook call at compile time, and checks that the typed
candidate reply preserves the task's original source and obligation.
Checkpoint release retains the issuer settlement, observer creation,
original-scope read/reply and final cleanup in one watchdog; the measured issuer
portion already took 538 seconds before those later phases. These outer process
limits preserve the tests' internal phase and cancellation assertions.

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

Every new prepared release also requires the `prepared-child` cohort from its
own frozen descriptor. The one-child control
`actor_host::prepared_runtime_acceptance::production_prepared_toolset_one_child_executes_original_native_probe`
is the preceding focused gate; schedule the twenty-child cohort only after that
control passes on the coherent candidate. Both use the same production fixture
and parameterized child program. This release gate is separate from the seven
M2 cases and is not part of routine focused spot checks.

```sh
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort prepared-child --output "$PREPARED_CHILD_EVIDENCE" --jobs 1 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

This cohort seals one exact ignored test and selects an isolated owned-resident
compiler. It refuses a different compiler mode or parallel test execution. One
executed passing test means that twenty sequentially admitted children completed
their actual native replies and the parent verified twenty typed answers; it is
not a claim of twenty concurrent children. The scenario requires twenty unique
actors and installation scopes. The root's typed `DeploymentOriginal` acquisition
must match the frozen workspace's completed inventory. Children reuse one actual
supplied spec, have explicit live installation origin without a source-prepared
acquisition receipt, and return typed answers of 41 through its native probe.
External-quoter execution bytes must remain unchanged from preparation before
its input changes from 41 to 42 through the parent's public spec import, every
child installation and every native reply. Each child setup and its attributed
compiled installer phase must submit zero compiler requests. Partial progress
and per-child rows retain observed counts on failure. The existing hosted
outcome records scenario and cleanup separately;
scenario completion alone does not establish confirmed host cleanup.

First preparation is reported separately. Setup P95 below one second remains
a measured target on the shared host, while the quotation, compiler, provenance,
reply and completion checks are required correctness conditions. A fixture-only
libtest from another revision may supply diagnostic evidence with its source and
hash recorded; it cannot replace the frozen libtest for release qualification.
Preserve older frozen descriptors and their reports unchanged when introducing
this new cohort; build and freeze the new candidate rather than editing an old
qualification contract.

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

The native assembly consumes the tracked `build/test-fixtures.json` manifest
and its declared Buck fixture tree. It checks their bytes against the native
source snapshot and copies them to `share/exomonad/test-fixtures/<relative
path>`. Freezing verifies this already assembled tree against the clean
recorded source and seals the manifest and per-file SHA-256 inventory into the
descriptor. The runtime environment binds `TIDEPOOL_TEST_FIXTURE_ROOT` to this
bundle-owned directory and clears any inherited value before execution. Frozen
verification rejects a changed manifest, missing or changed fixture, and extra
fixture file.
