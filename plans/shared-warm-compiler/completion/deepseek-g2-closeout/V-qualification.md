# V: joined evidence, map and fresh Persona

Primary outcome: qualify the actual joined candidate and one frozen bundle.
Secondary: correct the compiler map after changes, verify no obsolete access
route survives, and make all claims traceable to exact commands/artifacts.

## Prepare immediately

Read the current AGENTS, `docs/swarm-builds.md`, `build/package/README.md`, the
G2 contract and declared test targets. Browser inputs are required for
exomonad-host/bundle builds even if a selected test opens no browser. Coordinate
actual resources with S/R; do not start cold lanes from an old free-space number.

Historical command arrays are under
`/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/morning-review55-commands.json`.
Reuse their admitted recipe and selected resources, not their executable or
output directory. Read the current runner before supplying arguments. These
known target/count pairs are a starting point, not a substitute for discovery:

| Target | Expected executed cases |
|---|---:|
| `//exomonad/host:facade_introspection_service_test` | 1 |
| `//exomonad/actor:actor_notebook_contract_tests` | 4 |
| `//exomonad/actor:actor_compiler_world_queries_test` | 2 |
| `//exomonad/actor:actor_world_status_test` | 1 |
| `//tidepool/runtime:runtime_package_source_history_test` | 1 |
| `//tidepool/runtime:runtime_compiler_end_history_test` | 1 |

Use the checkout's `swarm-build just test-native TARGET` workflow, declared
owned-resident compiler resources, a fresh retained-output directory and the
required artifact-retention options. Named targets already select their cases;
don't accidentally stack a second incompatible selector. Resolve additional
cold/cutoff/private/reload selectors from actual targets and command manifests.

## Gates and batching

1. **Per candidate:** compile production plus directly affected cfg(test), Haskell
   and hosted consumers. Execute each repair's discriminating positive/refusal
   history. Linking or metadata generation isn't a test pass. Record actual
   selected/executed counts and first failure.
2. **Joined package/native/witness checkpoint:** run Discovery and all four
   notebook cases unchanged in semantic purpose. The latter need both W and N;
   treat progress to a new frontier as a failure with better diagnosis. Include
   P's producer→decoder→install control rather than waiting for a giant suite.
3. **Required G2 histories:** query/status, finite/historical/cold, actual eviction
   and reacquisition, private input, captured-A cutoff, ordinary reload,
   caller-loss and uncertain END. H supplies history-specific evidence; verify
   it applies to the joined source. Preserve refusal and cleanup controls.
4. **Serialization/translation changes:** run the affected declared fixture
   cohorts via `just fixtures-check` and cross-language tests. Missing compiler
   inputs are failures, not passing skips. New fixtures need their declared
   Buck resource inputs.
5. **One frozen bundle:** build the packaged compiler catalogue and runtime
   bundle, freeze through the existing qualification owner, then run the named
   non-cold Persona/G2 acceptance subset with those exact products. Host,
   libtest, compiler, stdlib, assets, profile and source must match its descriptor.
6. **Live smoke then user run:** verify Persona selection/tools/initial memory;
   run a bounded real DeepSeek multi-turn history with ordinary cells, retained
   use, discovery and compatible reload. Inspect actual provider/tool receipts.
   Start fresh conversation state for the user while preserving authored files
   and memory. Report live URL only after service readiness is observed.

Run independent admitted targets concurrently when resource measurements support
it. A full bundle per leaf wastes the shared budget. Once qualification freezes,
later unrelated C/quality proposals become separately published followups; only
a required repair restarts affected qualification. A known ordinary-workflow bug
still blocks G2, even if exploratory execution would produce interesting data.

## Persona source and credentials

A prior private review copy is
`/srv/swarm/checkouts/LettaPersona-g2-private-qualification-20261021` at
`e67e96c5caf3`; its existence is not acceptance or current user state. Read the
recorded private-copy handoff, inspect current authored files and memory, and
reconcile later edits before choosing a writable qualification descendant.
Never copy an old persona tree over newer user work. Use a private test port
and the existing launch owner for mutable qualification.

Use configured DeepSeek credentials through the existing owner. Do not print
keys, put them in Git, or copy them into evidence. Keep OpenAI selectable. Check
reasoning/tool continuation and explicit failure receipts; no silent retry or
generic profile fallback. No new provider architecture is part of this wave.

## Map and delivery review

Update `docs/compiler-work-map.md` and the completion verification ledger from
the final code and terminal receipts. Map the three changed seams explicitly:
who issues a choice/progress fact, how it travels, which consumer uses it and
which exact recovery path restores it. Remove obsolete scans/reconstruction
claims and mark remaining measured/unmeasured work. Do not call this a measured
speedup based only on smaller code or fewer lookups.

Review the joined source for surviving old fallback entrypoints, test-only
authority constructors, source-less native publication gaps and catalogue
overwrite paths. Reuse P/R reviews but inspect changed joined consumers directly.
Run current formatting, documentation/link checks and `git diff --check`.

Run `scripts/integration-status.py --strict` under the documented command owner;
report missing refs honestly. It reads cached refs, so also obtain remote
freshness explicitly. Push consolidated checkpoints to `github` and verify
exact OIDs with `git ls-remote`. Reconcile controlling integration/release/main
at a safe ownership boundary; don't switch another active checkout's branch or
invent a local main ref merely to satisfy a checker.

Final report: source/tree and remote refs; descriptor/executable hashes; exact
commands, counts and results; cleanup certainty; map and evidence paths; remaining
G3/C items and any unmerged candidate; actual Persona URL if deployed. Do not
collapse source-reviewed, compiled, executed, frozen-qualified and live-tested
into a single green label.
