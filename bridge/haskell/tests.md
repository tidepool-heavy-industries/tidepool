# Haskell test components

Buck owns test registration, generated inputs and runtime tools. The Cabal
test components describe the same source programs for local compatibility;
they are not an independent fixture or acceptance registry. Use the owning
repository frontend documented in `docs/swarm-builds.md` to select targets.

Each counted component exports `tests :: TestTree` and calls the shared
`Tidepool.Test.Runner.runTests`. Standard Tasty `--list-tests` discovers cases
and `--pattern` selects them. No pattern means all cases in the selected
component. An empty selection fails, including a filtered discovery request.
Compilation, discovery and executed cases are separate evidence.

The runner serializes selected leaves through Tasty's root scheduler option.
Cases have no artificial dependency edges: selecting a later regression does
not execute earlier regressions. Failures do not suppress independent cases.
Buck may run independent suites in isolated processes. QuickCheck trials are
property trials, not additional named cases.

Compiler scenarios receive immutable source trees, the matched worker and
production compiler configuration as declared inputs. Each scenario owns its
mutable scratch. The generated Effects directory is passed through
`TIDEPOOL_TEST_EFFECTS_DIR`. A missing declared input fails; it never skips a
case. Candidate fixtures capture actual GHC originals and use the production
Rust issuance owner. Typed structural fixtures do not claim compiler authority.

Child-process roles have separate executables:

- `declaration-join-consumer`, supplied as `TIDEPOOL_TEST_DECLARATION_JOIN_CHILD`;
- `source-boot-child`, supplied as `TIDEPOOL_TEST_SOURCE_BOOT_CHILD`;
- `worker-response-child`, supplied as `TIDEPOOL_TEST_WORKER_RESPONSE_CHILD`.

These are runtime dependencies, not counted test suites. The
`execution-corpus-producer` executable is a compiler action. Its mapping and
typed inventory checks belong to `execution-corpus-projection`; passing those
checks does not execute a source corpus. Compiler benchmarks and diagnostic
snapshot tools are separate optional executables and do not contribute test
counts.

The host-side Tasty environment is `buck-test-ghc`, with closure
`buck-haskell-test-closure`. It uses the same patched GHC and production package
set as `buck-ghc`, plus test providers. The deployed worker keeps its production
closure. Reconfigure the declared test toolchain after materializing its Nix
outputs; ambient Haskell packages are not action inputs.

Local Cabal compatibility enables child tools through `test-tools`; standalone
measurement tools use the `benchmarks` flag. The model test requires the actual
generated Effects and protocol output directories at `generated/effects` and
`generated/protocol`, relative to this directory. Supply explicit links to the
matching Buck outputs, with `Tidepool/Effects/Core.hs` and
`Tidepool/Internal/ModelControl.hs` at their module-relative paths. Missing
outputs are compilation errors. No stub modules or alternate generator are
provided. Native Buck compilation consumes the artifact providers directly.

The narrow helper contracts are independently selectable native suites:

| Target under `//bridge/haskell:` | Named Tasty cases | Retained checks |
| --- | ---: | --- |
| `native_helper_contract` | 17 | Native assertion, bounded observation and generated actor-cell behavior |
| `pinned_source_contract` | 1 | Actual facade pinned assertion with external `Ext.Tiny` |
| `automation_helper_contract` | 2 | Eight planning assertions and 26 generated Commands interpreter assertions |
| `browser_scenario_contract` | 3 | Original workflow matrix, protocol isolation, helper refusals and command specification |

Assertion matrices are checks inside named cases; they do not inflate the
runner's executed-case count. These four suites expose 23 cases. The separate
facade `facade_prepared_recipe_contract_test` executes eight prepared-runtime
cases in one counted test. `facade_recipe_source_capture_test` records seven
cells and four assertion labels in one diagnostic test; recording those labels
does not validate their behavior.

Workspace modules come from the workspace's source exports. The automation
suite's pinned Jev modules use declared `jev_sources` artifact projections.
The pinned contract compiles the owning facade fixture as `Project.Checks`,
while its mutable assertion cell sees the runtime fixture `Ext.Tiny`. Local
Cabal compatibility supplies the same graph outputs under `generated/jev`
and `generated/pinned`, alongside the Effects and protocol outputs above.
Missing generated sources fail compilation.
