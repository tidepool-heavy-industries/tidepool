# World native assembly findings

Source base: `2024a912f933d639b67576fda6e54460192551a8`, with the captured-observation
getter received as `2185497a45` and the program-basis contract as `dc3e53dd1d`.
This records source findings and local compiler-boundary evidence; it does not
qualify actor execution or the complete language contract.

## Confirmed and repaired in this change

- `tidepool/toolchain/src/artifacts.rs`, `PreparedCellProgram::seal`, previously
  lent the final accumulated declaration context and source lexical support to
  every native item. The preparation cursor already owned each item prefix.
  Preparation now retains that exact prefix next to each prepared output;
  sealing consumes those retained contexts in order. Compiler output type
  availability and runtime completed-value authority remain separate.
- `tidepool/toolchain/src/checked_cell.rs`,
  `PreparedCheckedItemOutput::capture`, reopened `turn.cbor` although
  `ProgramNativeOutput` already owned its captured turn. It now consumes those
  immutable bytes. World item receipts use the already authenticated original
  binder/entry proof; they never masquerade as finite checked-item receipts.
- World normalized items and captured outputs had no executable native consumer.
  `artifacts::prepare_world_cell_program` now enters the existing segment-original
  issuer, product item selector, native target issuer and checked item seal.
  `ExactCheckedCell` and `CellProgram` explicitly retain the World owner.
  No finite request or baseline is synthesized.
- `declaration_context.rs`, `ProductInputFacts::admit_source_facts`, required
  finite filesystem import roots even for captured World source claims. The
  World issuer now passes its authenticated `WorldSourcePathDomain` through the
  existing World source-selection validator. The finite validator is unchanged;
  captured aliases remain valid after transport directories retire.
- `compiler_world.rs` previously equated the native entry owner with the captured
  value-interface module. Actual output uses the generated scaffold for native
  entries and `Tidepool.Session.Val.G1` for the captured binder. Engine repair
  `98e66c5613` separates those observations. Native sealing still authenticates
  the exact original owner and ordinal against the same certified product;
  wrong-owner, sibling-ordinal and absent-ordinal controls pass.

## Confirmed external prerequisite

- The native BUCK generator initially refused the existing checked-request
  compile-fail contracts. The test owner supplied `c1e07eef5f`, received here as
  `54977fe2fd`; canonical generation then accepted the new World fixture.
- The rooted control exposed `source recipe compilation route`: Haskell
  `CompilerProducts.hs` emitted `Ordinary` because no finite exact request
  existed. H repair `ddce287fd9` carries actual World provenance in the existing
  private dependency sum. Receiver `baea57bd12` admits only the distinct
  `world-unavailable` recipe with the same actual base World identity and
  captured-source domain. The nested TPCERT10 tag has strict byte lengths and
  the explicit unsupported-recipe reason. It publishes no execution-source
  graph and does not reinterpret ordinary or finite-exact evidence.
- At this base the Haskell World producer lacks declaration/prologue original
  products and refuses these inputs. H owns that producer extension; native
  assembly must consume the actual authored declaration certificate when it
  exists. A native link or a discard-binding control does not qualify declarations,
  imports, produced binding reuse or publication.
- `compiler_world.rs` still contains the intermediate native-seal TODO and retains
  decoded raw item observations alongside normalized item proofs. The engine
  owns that file and will remove the obsolete TODO after receiving the consumer.
  Raw captured certificate bytes remain necessary evidence. The retained raw
  `WorldCellOutputAdmission.items` tree is read only for its length in admission;
  dropping that decoded copy is an engine-owned opportunity, not a measured
  memory improvement.

## Untested relationships

The actual actor two-cell history, per-item World successor adoption, post-removal
publication, source revocation at install, and lost-waiter cleanup remain the
engine and verification owners' joined gates. This compiler-only native adapter
issues no runtime lease or execution authority.

## Local evidence and investigative correction

At source `f60f6deed8` plus the source-domain and issuer-control changes, the
production target compiles and four focused controls pass (build
`1732a458-e116-4e9b-863b-08f299ac47a4`, exit 0). Log:
`/srv/swarm/evidence/inanna/native-world-entry-and-source-controls-20261020.log`.
The pre-recipe actual failure remains retained at
`/srv/swarm/evidence/inanna/native-world-after-entry-fix-20261020.log`.

At source `d946652328bad37d93cdc3fad6a81ccdd90db2db`, the named actual World
control passes 1/1, exit 0 (build `bfecec64-fd21-472f-b103-bf172df67024`):

```sh
swarm-build just test-native //tidepool/toolchain:toolchain_world_native_seal_test --output-dir /srv/swarm/evidence/inanna/native-world-explicit-recipe-20261020 --retain-artifacts
```

It prepares and seals a real captured bind after matching origin and Clean END.
A second genuine preparation cancels the same transaction-issued post-END
observer and refuses sealing with `Io(Interrupted)`. All six compiler operations
are retained as completed; owned compiler cleanup is confirmed with no diagnostic
issues or unmarked scratch. Log: `native-world-explicit-recipe-20261020.log` in
the evidence directory. The retained report directory has the same basename.
The test executable SHA-256 is
`009deb830139a9d7b6006b65ab46e1712f075f2d660b8d17ccc03662a6ca3d36`;
the Haskell worker SHA-256 is
`ccf0785498ea921f9d05536869f9ba151b7f2c1dfc509ad5fc8564e09ea7a11b`.

The strict World recipe codec, finite-exact sidecar ownership and native entry
issuer controls pass 3/3, exit 0 (build
`11a37d79-383f-401d-978e-117f089d44e6`). Log:
`/srv/swarm/evidence/inanna/native-world-recipe-controls-final-20261020.log`.
These are compiler/native evidence checks, not actor execution qualification.
The joined source-domain controls also pass 3/3, exit 0 (build
`41bc805e-e276-4e25-b174-5bea0e43a2b7`), covering captured alias lifetime,
envelope bounds, and named-reference refusal. Log:
`/srv/swarm/evidence/inanna/native-world-joined-source-controls-20261020.log`.

The actual run retains all three completed compiler transaction directories and
reports no scratch-marker issues. An initial scratch inspection omitted hidden
`.tmp` paths because it used `rg --files` without `--hidden`; this was an
investigative error, not a runner environment or retention defect.
