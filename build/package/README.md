`//build/package:native_runtime_bundle` is the prepared release target. It
carries Buck's host, libtest, extractor frontend and worker, declared shared
libraries, browser assets, the authenticated native catalog and a complete
original root entry. It requires the retained source and compiler selection.
Configure with `bash scripts/buck2-configure.sh --tests --browser` before
bundle assembly and qualification; the bundle owns the browser driver dependency.

`//build/package:native_catalog_runtime_bundle` selects the same prepared
assembly. `//build/package:source_backed_developer_bundle` is an unprepared
developer path and does not establish prepared startup acceptance. Build, freeze
and consumer qualification of one exact prepared bundle remain required before
delivery.

The package, isolated runner and performance reporter mutation controls run
through their declared source snapshot, including the original roster and
compiler trace fixtures:

```sh
swarm-build bash scripts/buck2-run.sh run --local-only -c remote.enabled=false \
  -c tidepool.profile=fast-dev //scripts:native_bundle_qualification_tests
```

These script controls complement actual frozen consumer execution.

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

The same renderer produces `TidepoolPreparedWorkbench.hs` and
`TidepoolPreparedAsyncWorkbench.hs`. They call the two existing default policies
specialized to the generated standard ordered actor row in
`Tidepool.Agent.Contract`, with no notebook-driver or workspace imports.
`//build/package:native_workbench_entry` and
`//build/package:native_async_workbench_entry` use the same retained source,
compiler and production-entry producer as the root entry. Qualification binds
both complete original containers and supplies `TIDEPOOL_PREPARED_BUILTIN_ENTRIES`.
Workspace overrides, unmatched ordered rows and explicit live policies keep
their existing routes.

Named source snapshot metadata is version 2, qualification descriptors are
version 3, and prepared workspace selections are version 8. Rebuild, refreeze
and reprepare with this reader; it does not infer missing named-entry provenance.
Pointer version 2 and native catalog version 4 retain their existing contracts.
Production entries use version 3. Preserve older frozen bundles with their own readers.
Immediate preparation/start carries only issuer-owned immutable entry/images;
each actor still installs fresh policy state. Durable reopening authenticates
the complete selected original through the existing loader.

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

Keep these values identical for every concurrent Buck client in one checkout.
Buck synchronizes daemon state when a client switches command-line
configuration; a client with only the ordinary profile settings can wait for
the retention-configured build to finish. An opt-in private JSON object can
carry the same settings into every `scripts/buck2-run.sh` invocation by setting
`TIDEPOOL_BUCK_CONFIG_FILE` to its path. Use the exact source root, record path
and record JSON selected for this build. The launcher checks that the file is
owned by the current user, mode 0600 or stricter, no larger than 64 KiB, and
rejects conflicting command-line values. This is scoped to the command
environment; it does not change `.buckconfig.local` or apply a server-wide
concurrency setting. Without one shared config file, run the retention-bearing
build and other Buck clients sequentially.

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

Production entries use schema 3, with an explicit native-catalog or frozen
workspace source selection. Native root and built-in entries own their complete
original output and declare no catalog dependencies. Workspace entries may link
to an exact authenticated catalog selection: fresh outputs belong to the entry,
while unchanged originals remain owned by that catalog. Reopening validates the
recorded catalog identity and selected original closure; it does not select from
the catalog's broader inventory or recompile source. Retain the linked catalog
and its source roots for the lifetime of the workspace entry.

Schema 1 and 2 entries must be rebuilt with the matching producer. Publication syncs
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

Build the native bundle in one selected native profile. It owns the declared
`//build/testing/browser:driver_bundle` dependency. Retain the
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
The browser driver and its locked npm inputs are assembled by the declared
native bundle action and checked against its source contract. Freeze cannot
substitute a separately supplied driver. Schema 3 descriptors require that
contract; historical frozen bundles retain their own qualification program.

Every runtime cohort seals `owned-resident` compilation into its descriptor,
matching the resident daemon used by CLI launch. A direct override, missing mode,
or unknown mode refuses before execution. The general isolated unit runner still
defaults to direct compilation for process isolation; release qualification does
not inherit that default. The separate catalog consumer keeps direct compilation
inside its cold namespace and refuses an existing daemon. Preserve older frozen
descriptors and their reports with their bundled qualification program; the new
reader refuses older unsealed cohort contracts. Build and freeze a new candidate
to qualify the resident runtime contract.

The `builtin-startup` cohort runs one ignored production-path case. It prepares
the default workspace, launches through immediate immutable readiness and then
independent durable selection, and checks zero startup compiler requests in both
hosts. Real HTTP admission drives first and warm notebook calls; a binding from
the first actor must be unavailable in the second actor. Run it with `--jobs 1`
from the rebuilt descriptor that selects both packaged installer entries.

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
The M2 roster also checks host shutdown and coordinator failure after child
failure. The `unified-regressions` cohort runs the published lookup/form examples
and all three operation-settlement controls through the same frozen libtest.

Every new release also requires `result-delivery` from the same descriptor.
Its six model-free resident cluster cases assert actual session placement and
force real Haskell values: a dedicated nominal scalar reply, a dedicated callable
reply after the child machine is gone, shared and dedicated pending-request
custody histories, a dedicated callable typed exit and replacement, and a
dedicated collector of real progress and settlement publications. Callable
reads check repeated observation, receipt execution/worktree equality, stale
request refusal and saved watch values after forgetting the request and watch.
The shared custody control retires the producer actor while retaining its shared
machine; the dedicated control requires the producer machine to be gone before
late forcing. These cases use genuine compiler-issued sites and typed inputs.
They establish resident result delivery; M1 and M3 retain their browser and
scripted-provider acceptance contracts.

The descriptor seals each case's placement, input path and forced-value premises.
The existing cohort report marks those premises verified only for one passing,
executed receipt with confirmed process, campaign and compiler cleanup. Missing,
duplicate, zero-count, unknown, failed or unconfirmed receipts keep the cohort
unqualified and remain in its reports. The cohort retains diagnostic artifacts,
uses a 1200-second watchdog per case, and permits at most two bounded processes.
Historical M2 acceptance keeps its original roster and meaning. The separate
ignored shared/dedicated native-renderer controls remain focused diagnostics;
they cannot share a selection with the nonignored result-delivery roster.

```sh
DESCRIPTOR="$FINAL_BUNDLE/share/exomonad/qualification.json"
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort m2 --output "$M2_EVIDENCE" --jobs 3 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort m1 --output "$M1_EVIDENCE"
python3 "$FINAL_BUNDLE/share/exomonad/qualification.py" run "$DESCRIPTOR" \
  --cohort result-delivery --output "$RESULT_DELIVERY_EVIDENCE" --jobs 2 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

For the parallel command, `ADMITTED_USER_SLICE` names an existing user slice
whose resource bounds have been checked for the chosen concurrency.

To cancel a running qualification, use the qualification owner's `cancel`
command with the private `run-owner.json` receipt. It checks the owner's boot
and process-start identity, then signals that exact opened process through a
Linux pidfd. The owner forwards the request to the isolated runner, which owns
the queue and stops scheduling while it cleans up its active exact case. Use
the wrapper so both the qualification owner and runner retain the cancellation
and cleanup evidence.

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" cancel \
  "$M2_EVIDENCE/run-owner.json"
```

Wait for the qualification command or its process supervisor to exit, then
inspect `run-owner.json` and `report.json`.
The private receipt binds the descriptor hash, source revision and owner/runner
PID start identities. It records forwarded signals, runner exit/reaping, case
cleanup observations and any retained interruption confirmation. Treat missing
or unconfirmed cleanup evidence as unknown; an owner process exit alone does
not establish descendant cleanup. If the command reports that the owner is no
longer running, inspect its retained result instead of retrying against a PID.

Every new prepared release also requires the `prepared-child` cohort from its
own frozen descriptor. The one-child control
`actor_host::prepared_runtime_acceptance::production_prepared_toolset_one_child_executes_original_native_probe`
is the preceding focused gate; schedule the twenty-child cohort only after that
control passes on the coherent candidate. Both use the same production fixture
and parameterized child program. This release gate is separate from the nine
M2 cases and is not part of routine focused spot checks.

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort prepared-child --output "$PREPARED_CHILD_EVIDENCE" --jobs 1 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

This cohort seals two exact ignored tests and selects an isolated owned-resident
compiler. It refuses a different compiler mode or parallel test execution. One
case proves that twenty sequentially admitted children completed
their actual native replies and the parent verified twenty typed answers; it is
not a claim of twenty concurrent children. The other case prepares the shipped
default workspace, resolves native lookup signatures, imports its actual
`AgentSpec`, and verifies a captured-context child's reply from a retained parent
binding. Both cases must execute and pass. The twenty-child scenario requires
twenty unique actors and installation scopes. The root's typed `DeploymentOriginal` acquisition
must match the frozen workspace's completed inventory. Children reuse one actual
supplied spec, have explicit live installation origin without a source-prepared
acquisition receipt, and return typed answers of 41 through its native probe.
External-quoter execution bytes must remain unchanged from preparation before
its input changes from 41 to 42 through the parent's public spec import, every
child installation and every native reply. Each child setup and its attributed
compiled installer phase must submit zero compiler requests. Each child's
provider-visible preview must match its distinct `NativeInput` ordinal.
The home-module `Display`/`WorkbenchDisplay` instances exercise original
source and literal images: one complete retained image bundle serves every child,
and each renderer invocation must install its target with zero successful native
image constructions. The ordinary one/twenty-child gate retains the production
shared-machine placement. The separate
`production_prepared_toolset_twenty_distinct_machines_execute_original_native_probe`
gate selects the existing dedicated-machine capability using the owner's root
bootstrap, and requires a separately issued machine session for every child.
The focused
`source_prepared_toolset_two_children_share_distinct_native_renderer_inputs`
control uses the same scenario with source preparation; it does not qualify a
frozen release. Partial progress
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

The `m3-recursive` cohort selects two scripted-provider acceptance cases. The
recursive case exercises this ancestry:
an actual Sol 6.1 root requests a Luna child, which requests a Luna grandchild.
Both ancestors remain pending while the grandchild's provider response is held.
The child and grandchild execute the root's captured native helper, and typed
replies carry its value of 42 back to the root. The case checks exact actor
ancestry, inherited context, distinct fork worktrees, a leaf effect-row refusal,
and confirmed descendant and host cleanup. The asynchronous case parks two real
notebook calls before a child reply arrives. The parent's typed watch must
receive the first accepted failure value while a later success reply, notebook
completion, and provider completion settle through the hosted runtime. Both
cases require confirmed host and descendant cleanup. Provider replies alone are scripted.
It uses the same frozen bundle, prepared catalog/root entry, owned resident
compiler and counted runner as the other runtime cohorts, serially with one test process.
It does not invoke the performance reporter. Older frozen descriptors retain
their own readers and rosters; rebuild and freeze the candidate that adds this
cohort.

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort m3-recursive --output "$M3_RECURSIVE_EVIDENCE" --jobs 1 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

The supplementary `harness-performance` cohort selects the one ignored
eight-phase HTTP/Engine/Store/notebook workload. The separate
`harness-performance-three-actor` cohort selects the scripted parent/two-child
capture workload. Both require the frozen prepared catalog/root entry, an
isolated owned resident compiler, one test process, and retained artifacts. Only
provider replies are scripted. The frozen owner runs its bundled
`harness-usecase-perf-report.py` after the counted test and writes
`harness-usecase-perf-report.json` beside `report.json`.

Performance cohorts default to `--trace-profile minimal`: phase outcomes,
exact compiler request/service joins, per-call timing, validation summaries,
native-image decisions, successful Cranelift compile summaries and machine
publication records remain enabled. `TIDEPOOL_TIMING_SUMMARY=1` is forwarded
through the owned compiler; per-file/module timing and host span lifecycle events
remain disabled. `--trace-profile full` is an explicit diagnostic control for a
bounded selected cohort, with detailed attribution enabled. Primary paired
latency repetitions use minimal; detailed captures do not replace those results.

`owned_artifact_observations` separates GHC frontend summaries, native image
production and machine publication. Image IDs are process-local, registry entry
IDs identify exact equal keys while retained, and expired weak images may be
compiled again. After a key is pruned, a new entry cannot prove equality with an
older entry. Validation intervals are joined by physical request identity;
buffered logger arrival order is not an execution boundary. RTS deltas measure
process allocation, not retained heap; overlapping allocation deltas are not
summed. Absent summaries remain unknown rather than zero work.

Behavioral and measurement outcomes remain separate. `behavioral_completed`
requires the one executed passing case and controls the cohort exit status.
The measurement report separately records phase coverage, exact workload
request/service joins with one reconciled owner per physical submission, whole
observed physical-stream reconciliation, nonnegative integer phase measurements, raw trace
capture versus truncated detailed samples, queue admission evidence, explicit
startup ownership, cleanup, and descriptor/profile identity. Missing or
ambiguous joins remain partial; every queue event counts, including invalid or
duplicate records. The selected cohort seals its named workload roster, rather
than accepting a shortened roster from observed output. Assembly retains the
three-actor roster as `share/exomonad/three-actor-workload-roster.json` from its
exact declared native source input. Assembly, freezing and frozen verification
check that asset against the build source contract. Bundle resource tests must
use the production assembly/copy owner; manually adding a reporter dependency
to a test bundle does not establish that assembly delivers it. Root startup
attribution requires the production preparation owner or canonical `actor_path = "root"`;
missing actor paths remain unknown. Queue or startup evidence is not inferred
from counts or timestamps. `measurement.completed` becomes true only when every
required evidence dimension is complete. It imposes no latency threshold and
does not establish provider network performance. These supplementary cohorts do
not block M2 acceptance.

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" run "$DESCRIPTOR" \
  --cohort harness-performance --output "$HARNESS_PERFORMANCE_EVIDENCE" --jobs 1 \
  --delegated-service --service-slice "$ADMITTED_USER_SLICE"
```

The catalog acceptance route requires the frozen descriptor. It verifies the
exact native selection before entering the consumer namespace, exposes the
original frozen bundle at its canonical path, and runs the one mandatory
catalog consumer in fresh caches with checkout and Buck build inputs absent.
The report requires one executed passing test; compilation or an empty
selection cannot satisfy it.

`NativeRunnerResources` in the bundled qualification owner declares path inputs
for both cohort and catalog namespace launches. The isolated runner resolves
those declarations and clears ambient compiler selections. Keep this policy in
the bundle owner; a source helper cannot replace an older frozen bundle's policy.

```sh
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" catalog-gate \
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
python3 "$(dirname -- "$DESCRIPTOR")/qualification.py" exec \
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
