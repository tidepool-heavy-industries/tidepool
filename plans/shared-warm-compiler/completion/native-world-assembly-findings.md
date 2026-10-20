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
- The actual rooted World control now passes output admission and closes with
  Clean END, then refuses at `certified_products.rs`, source recipe admission,
  with `source recipe compilation route`. Haskell `CompilerProducts.hs` emits
  `Ordinary` when no finite exact request exists, while World correctly supplies
  an exact World source admission. H/root own the explicit World recipe contract;
  native assembly must not fabricate a finite request or relabel `Ordinary`.
- At this base the Haskell World producer lacks declaration/prologue original
  products and refuses these inputs. H owns that producer extension; native
  assembly must consume the actual authored declaration certificate when it
  exists. A native link or a discard-binding control does not qualify declarations,
  imports, produced binding reuse or publication.
- `compiler_world.rs` still contains the intermediate native-seal TODO and retains
  decoded raw item observations alongside normalized item proofs. The engine
  owns that file and will remove the obsolete TODO after receiving the consumer.
  Raw captured certificate bytes remain necessary evidence; a second decoded
  item tree is a separate potential retention cost, not yet measured here.

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
The actual World control is still one executed failure, not a successful seal:
`/srv/swarm/evidence/inanna/native-world-after-entry-fix-20261020.log`.

The actual run retains all three completed compiler transaction directories and
reports no scratch-marker issues. An initial scratch inspection omitted hidden
`.tmp` paths because it used `rg --files` without `--hidden`; this was an
investigative error, not a runner environment or retention defect.
