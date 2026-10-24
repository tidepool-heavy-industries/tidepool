# Astra handoff: first actor notebook demand after accepted compiler repairs

## Decision and source

User asked to attempt G2 after Argon accepted the import / retained-slot /
issued-row repairs. Qualification exposed the new boundary below. User now asks
for hypotheses and notes for an Astra pass. No production repair has been made
for this boundary. Do not resume broad speculative refactoring before locating
where the exact group/owner association disappears.

- Root: `/srv/swarm/checkouts/tidepool-domain-runtime-join-20261023`, branch
  `integration/g2-domain-runtime-20261023`.
- Accepted production: `75a0aa0fed3e30745ff43053d9ed5ea1a0f7cd72`.
- Qualification checkpoint: `98d1d08198a5600c595414d85658982f52046dd1`.
  Production unchanged; the only test-code change adds the missing ignore
  annotation to the rooted retained-command actor case. Other changes are docs.
- Validator: `/srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020`.
  Pinned clean at98d1. Do not advance while gate processes hold the source pin.
- Evidence root E:
  `/srv/swarm/evidence/inanna/g2-import-ownership-closeout-20261024/`.
- Current process/status ledger: `E/g2-qualification-state.json`.
  Exact invocations, exits, hashes and source-before/after: `E/*.command.json`.

## Confirmed failure, not a hypothesis

First case in `//exomonad/actor:actor_notebook_contract_tests`:
`resident_local_actor::completion_progress::parked_cell_does_not_block_another_calls_acknowledgement_or_control`.

It successfully installs the actor, then fails on the first notebook seed:

```haskell
x <- pure (0 :: Int)
```

`ConcurrentResident::read` at `exomonad/actor/tests/resident_local_actor.rs:1049`
receives `KernelWorkbenchFailure`, phase Compile, class VersionSkew,
`NotPublished`, no item receipts. Semantic cause:

```text
ArtifactInventoryFailure::NativeGroupUnavailable
artifact = 3cbd1f3e365573b967e2cb13f6d43fde7f6940fb4ecc46ff02c75f7856f5a4a8
original_ordinal = 46
native_refusal = None
execution = Received { success: false }
```

This is before the test starts its parked command. Thus it has not tested
acknowledgement/cancellation concurrency yet. No claim of a scheduler regression.
All five cases executed and failed at the same group46 boundary, with differing
artifact IDs in independent compilations. All five confirm process/compiler
cleanup; hosted cleanup is not observed and diagnostics are marked incomplete.
The gate exited1 with source unchanged. See `E/g2-actor-frontier-results.json`.

Case1 evidence ID:
`3585106ed2f70a22d577957c616b0630e8c353ee8baa7e2bf494b03e7c91f062`.
Under `E/g2-98d1-actor-notebook-contracts/`, its sibling `.json` and
`.stderr.*.log` retain result and failure. Its directory contains
`artifacts/compiler-transactions/`, abbreviated T below.

## Decoded evidence: bytes exist; exact available authority still uncertain

- `T/.tmpsqZzE7/`: successful initial actor compilation. Has
  `module-products.cbor`, `certified-products.cbor`, finalized interfaces/Core,
  `turn.cbor`, consumed-source evidence.
- `T/.tmpFZONzP/`: World source capture, `world.cbor`, `sources.bin`.
- `T/.tmpkOQ9NL/`: successful cell planning.
- `T/.tmpIC46jQ/`: failed stage0. Retained generated Haskell contains the exact
  seed above. `compiler.stderr` reports repeated NativeDemandRejected after the
  first refusal; repeated text is not evidence of several independent causes.

Root decoded `world.cbor[10][30][1]` (the inner byte payload):
`TPCWPRIMARY2` names `main:Tidepool.Aeson.Value`, canonical artifact
`33db21d3e3ed4306e08e90bf111fe8a39bc335d7ad3f59b7f5bdf9ff34e40fed`,
and Original artifact `3cbd1f...` above. Both bindings carry selection
`5cc72c404be8549000dc197d6403d676a8e255c19f788f9df4a9bade8ea57363`.

Crucially, initial `module-products.cbor[2][8][3][46]` is a complete TPGRP byte
payload for `main:Tidepool.Aeson.Value`, ordinal46, binder `$fToJSONBool`.
SHA256 of those exact bytes:
`1cd0c503dbfe10b96c58b0e44b0edf981a120093003b2152dd32d19d016fdd66`.
That module has198 group payloads. Therefore "GHC never generated group46" is
not a good leading hypothesis. Byte presence does not itself grant execution.

In `certified-products.cbor[2][1][8]`, group46 has import index826;
`[2][4][826]` names `$fToJSONBool_$ctoJSON`, from Source8/group45. This is
an import of the dictionary's method, not evidence of an off-by-one bug.
Do not confuse physical group ordinal, global import index and product-array index.

The bounded private decoder reused for inspection is
`/srv/swarm/evidence/inanna/projection-executable-source-custody-20261004/inspect_cbor.py`:
load `Reader` with runpy; require complete byte consumption. `world.cbor`,
`module-products.cbor` and `certified-products.cbor` are directly CBOR;
`meta.cbor` has the eight-byte TPLR header. `world[10]` embeds additional CBOR
payloads as bytes. Never print whole module byte strings; inspect coordinates.

## Leading hypotheses and discriminators

1. **SOURCE metadata observation demands an acquired execution-group row too
   early.** `compiler_world/native_demand.rs:795` dispatches SOURCE queries to
   `SourceDependencyBasis::observe`. That validates exact pair/source membership,
   then `declaration_context/source_dependency.rs:94` calls
   `view.acquired_original_group_for_carrier(original, ordinal, operation)`.
   If the actor World retained nominal Original/source custody plus a latent
   offer, but no already-issued group46 node, this lookup can fail before actual
   native acquisition. The later code walks exact group imports to discover
   retained-value dependencies, so replacing it with empty dependencies is unsafe.
   **Discriminator:** identify the failing query kind and first refusing call;
   compare authenticated SOURCE pair, available offer, acquired group nodes and
   selected execution roots at that moment.

2. **An issued group exists but the World view lost its exact carrier edge or
   narrowed its custody.** `NativeSelectionIndex::resolve` around
   `artifact_inventory.rs:6290` and `resolve_group_binding` around6866 emit the
   same plain NativeGroupUnavailable. The latter requires an exact interface
   edge from Group to the requested ArtifactBinding, not just equal ArtifactId.
   **Discriminator:** trace group46 from original issuance through capture,
   materialization, detached request and World-primary construction. Compare the
   actual binding/selection and exact edge at each boundary. Do not infer that
   physical equality establishes same issuance.

3. **Native availability dependency resolution loses a carrier association.**
   The emitted-dependency resolver / `NativeDependencyCatalog::group_supports`
   in `declaration_context/native_availability.rs` around1021/1060 has another
   possible plain-error path. This is less localized than hypothesis1 so far.
   **Discriminator:** observe whether SOURCE completes and a Native packet is
   actually reached before failure; then inspect candidate's exact dependency
   carrier rather than collecting more unrelated inventory.

The receipt lacks typed native_refusal, which narrows likely constructors but
is not a stack trace. Do not claim hypothesis1 confirmed solely from matching
error text. This is not established as a regression introduced by A/B/C; earlier
regional histories did not exercise this same actor-installed library surface.

## Best next action

Use existing tracing at the SOURCE/native owner and exact group lookup before
adding diagnostics. The first run used trace-profile minimal with compiler
artifact capture. One exact rerun with `--trace-profile full` and bounded owning
`RUST_LOG` targets can discriminate the above after the current cohort settles.
If current events cannot identify the first refusal, add structured, filterable
fields there: query kind, carrier binding, ordinal, source-membership owner,
view group presence, exact carrier edge, offer presence. Do not println debug or
turn failure strings into control flow. Retain successful prior refusal evidence.

Once located, prefer a typed handoff carrying the correct existing owner. If the
SOURCE observation legitimately needs latent groups before native selection,
clarify how dependency observation uses authenticated available facts without
creating execution permission. Do not solve it by accepting all bytes, unioning
all inventory roots, reissuing nominal metadata or weakening historical checks.
Use the first actor cell as a production counterexample and keep a refusal
control proving custody alone cannot execute an unissued group.

Read the independent worker note at `E/actor-group46-source-hypotheses.md` when
present. `/root/impl_materialized_rows` was asked to stop read-only at this
handoff, not implement. No new source repair is authorized by that worker task.

## Qualification state and remaining work

The original75a0 gate built successfully but executed zero cases because one
of five selected ignored tests lacked #[ignore].98d1 repaired that exact
classification; runner guards were not weakened.35 named G2 target rosters were
statically audited afterward; no second ignored-mode mismatch found. The actual
current semantic/publication counts are30/34, not stale prose22/32.

Root tool session97907 completed: the five-case notebook gate failed5/5.
Session42877 remains active: seven exact compiler-free codegen ownership
controls, currently building dependencies after the notebook released the output
lease. These did not execute in parallel with the notebook gate.
Inspect their retained command records/exits and reports before changing source
or stopping the daemon. Do not restart a duplicate invocation or delete evidence.

All three accepted joined runtime histories and regional controls remain valid
within their recorded scopes. G2 is NOT qualified. No new bundle, Persona,
DeepSeek smoke, main merge or release claim. Remaining gates are in
`plans/shared-warm-compiler/completion/deepseek-g2-closeout/TEST-GATES.md`;
`E/g2-remaining-gates.json` is only a planned roster, not an executed queue.

The separate Nix2.31.3 root-scanner issue remains unresolved; current failed
actor cases got past startup and failed inside compilation, so do not attribute
this new failure to Nix. Serial startups do not prove safety against external
scanners. No root verification bypass or Nix/system change was made.
