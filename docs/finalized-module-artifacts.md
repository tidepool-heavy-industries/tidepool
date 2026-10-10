# Canonical finalized module artifact contract

One GHC finalization owns the canonical skinny interface and its matching tidy
Core. Preparation derives native products from that result. Checked-only HPT
interfaces are provisional and cannot issue this certificate.

## Worker receipt

`TPCERT9` is exactly nine CBOR fields:

```
["TPCERT", 9, nativeModules, targets, packages, globalDictionary, envelope, sourceRecipe, ownerCoordinates]
envelope = ["tidepool-ghc-finalized-module-v1", homeUnits, finalizedModules]
```

Each global dictionary row carries one exact symbol, representation, optional
entry signature, evaluated requirement and a closed owner reference. Source
owners use `["source", coordinateIndex, ordinal]`; package owners use
`["package", coordinateIndex]` or `["retained-package", coordinateIndex, generation]`;
retained home owners use `["retained", generation]`. The owner's binder is the
same exact symbol already carried by the global. Source coordinates are
`["source", unit, module, optionalVersion]`; package coordinates are
`["package", unit, module, interfaceSHA]`. Source coordinates remain independent
of the symbol's defining module. Wrong coordinate kinds, invalid indices,
duplicate rows and unreferenced rows are refused. The wire remains bounded to
4 MiB, the reconstructed unique full global dictionary to 4 MiB and expanded
group/target witnesses to 16 MiB and 65,536 references. Earlier receipt versions
are refused; durable home-owner and prepared artifact formats do not change.

`sourceRecipe` is the closed ordinary/exact-unavailable/exact-available result
documented in `bridge/haskell/CLAUDE.md`. Exact available receipts bind the
fixed owning-output `execution-source.cbor`; source-import package evidence is
separate from the native-global `packages` field.

`homeUnits` is the complete sorted unique GHC home-unit inventory from the
finalizing compiler environment. Rust never infers home status from `main` or
another unit spelling. Native owners and every home requirement must belong to
this inventory; package witnesses must belong outside it.

Each finalized module row has exactly eleven fields, sorted by `(unit,module)`:

```
[unit, module, sourceSHA,
 interfaceRelativePath, interfaceSHA, interfaceBytes,
 packagesRelativePath, packagesSHA, packagesBytes,
 coreDescriptor, interfaceRequirements]
coreDescriptor = null | [relativePath, SHA, bytes]
interfaceRequirements = [[requiredUnit, requiredModule, interfaceSHA], ...]
```

Requirements come from the actual finalized `ModIface.mi_usages`, including
interface-only home imports. They are sorted unique exact seals; source SHA or
ABI equality cannot replace them. Source SHA comes from consumed source evidence
for the same exact owner. Fresh native receipt owners must match their finalized
row and have captured Core. A `retained-core` native row instead requires an
admitted exact canonical original with its original certificate and captured
Core; it cannot issue a current source row or source execution recipe. `null` expressly has no source-free recovery input;
later native demand refuses instead of compiling from source.

Retained native versions use the `retained-core-home-v1` domain and the finite
reachable graph of actual retained groups. Each node binds the original canonical
certificate, singleton native product and package sidecar digests. Local promoted
edges name nodes and original ordinals; external native edges bind complete
resolved owners, while retained and package edges bind their generations and
exact selected interface seals. Sorted node traversal handles cycles and excludes
unrelated request context. Haskell and Rust derive this identity independently
from existing validated witnesses; it creates no additional authority store.

Paths name distinct captured files beneath the worker output directory. No
absolute, parent, current-directory, symlink or aliased payload path is admitted.
Digest and declared length are checked on captured bytes. Interfaces/Core are
individually limited to 32 MiB, package witnesses to 4 MiB; receipt metadata is 4 MiB,
128 modules/home units, and aggregate captured finalization payload is 128 MiB.

The profile names the matched compiler's canonical frontend rules. The Rust
issuer pins that profile and binds its configured endpoint producer SHA; the
worker cannot choose another producer by writing receipt text. Producer identity
continues to bind the declared compiler/toolchain deployment. Request-dependent
compiler inputs and finalized dependency selections remain evidence, not source
hash equivalence.

## Durable certificate

The Rust issuer encodes one canonical certificate per finalized module:

```
["TPFINALMODULE", 1, profile, producerSHA, homeUnits,
 unit, module, sourceSHA, interfaceSHA, packagesSHA, coreSHAOrNull,
 interfaceRequirements]
```

Scratch paths do not participate in durable identity. Certificate SHA participates
in the canonical interface artifact descriptor. Payload bytes and validated facts
share one privately constructed immutable carrier; cold recovery fully decodes
and validates the same certificate and payloads. Core has separate captured-file
materialization under the existing artifact owner, not inline receipt storage.

## Inventory authority

The canonical interface is the sole exact-owner index target. Native implementation
nodes depend on that interface and on their exact native implementation children.
No interface-to-native edge exists. Adding a native implementation leaves every
previous interface-only root and its authority unchanged. Lexical visibility,
native execution grants and compiler Core-recovery admission remain separate.

## Exact compiler scope

`TPEXACTSCOPE12` has eleven fields: the seven declaration/interface/native fields, execution descriptor or null, purpose authorization or null, the published source-original selections, and the input acquisition. Acquisition distinguishes fresh file authentication from continuation of owned original images; each receiving scope retains its own byte allowance, selected aliases and protected origin observations. Each published row binds an exact public root to its canonical interface, native product, source revision, original input identity and selected closure digest. The host issues these rows from sealed completed output; recovery restores their policy only through the checked durable graph and authenticated immutable inventory. Ordinary imports still demand current source. The typed `reload-inspection1` purpose forces candidate-source validation for the retained original namespace, including published roots, independently of the inspection query. Request authorization retains the strict `request-types2` wrapper and its explicit helper recipe. Each interface row has eight fields: the existing seven plus a typed role: `["value"]`, `["join"]`, or `["module", certificatePath, certificateSHA, corePathOrNull, coreSHAOrNull]` for a canonical module. The role comes from the admitted artifact kind; null and spelling-based classification are rejected. The two Core fields are both null or both present. Core is a compiler input; it carries no lexical selection or executable lease.

`TPHOMEOWNERS5` adds the exact finalized-module certificate SHA as its ninth field. A native original admitted to the inventory must have this binding and the matching validated carrier. Null is permitted only for standalone ownership encoding without canonical module custody; it cannot admit a native original. The issuer chooses the execution digest and module certificate binding before encoding once. Recovery decodes and validates both durable certificates and all selected bytes.

The existing `TPMCAN10` record references the same captured canonical certificate and Core through `RecoveryModuleInterfaceRef`. Publication completes those captured files before exposing the record. Candidate selection validates the configured producer, source digest, exact native owner, canonical certificate and payloads; the candidate retains the resulting immutable typed carrier. There is no alternate cache for type-only modules.

The inventory retains immutable variants by content ID. Each selected sealed closure has one canonical interface ID per exact owner and an exact native child index keyed by unit, module, version, interface SHA and product SHA. Admission and merge validate that selected closure before mutation; unrelated retained views cannot supply dependencies or conflict with its owners. Native children point to the exact canonical interface carried by their product. Interface projections select canonical IDs and cannot reach native children. A later native admission cannot alter an earlier view's outgoing edges or broaden its roots. Multiple native implementations may coexist only when their canonical carrier ID agrees; compilation materialization refuses an ambiguous native owner because the request wire selects one implementation per owner.

The worker candidate manifest is strict `TPMCAN10`: seven outer fields (the existing six plus the configured canonical producer SHA), with sixteen fields per candidate. Fields 14 and 15 retain the exact canonical requirement keys and `["module", certificatePath, certificateSHA, corePath, coreSHA]`. Native candidates require Core. Selection repeatedly removes candidates whose sealed dependencies are absent or differ from the actual exact-view interfaces and surviving candidates, recording a typed refusal; it never derives interface authority from source imports.

Deployment catalogs use strict schema 4. The source selection binds the original retained snapshot, ordered stable-effect, stdlib, actor and Jev roots, and complete source manifest. Source witnesses are ordered `{path, sha256}` records for `.hs`, `.hs-boot`, `.lhs` and `.lhs-boot` files; both Rust admission and Python qualification compare them with actual original bytes. The unpublished tuple witness format is refused and must be regenerated. Generic source/cache identities remain BLAKE3. Every native module row retains its canonical interface certificate and Core reference through the existing recovery-file owner, alongside its native ownership seal. Export refuses missing canonical custody; cold loading validates both certificates, the exact full native owner, producer/source binding, and payload checksums before offering a candidate. Canonical payloads charge the existing aggregate budget. A complete product container can relocate without changing original source paths or proof bytes. Earlier catalog schemas require regeneration through the deployment producer.
