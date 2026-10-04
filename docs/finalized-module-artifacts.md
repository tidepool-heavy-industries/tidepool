# Canonical finalized module artifact contract

One GHC finalization owns the canonical skinny interface and its matching tidy
Core. Preparation derives native products from that result. Checked-only HPT
interfaces are provisional and cannot issue this certificate.

## Worker receipt

`TPCERT7` is exactly eight CBOR fields:

```
["TPCERT", 7, nativeModules, targets, packages, globalDictionary, envelope, sourceRecipe]
envelope = ["tidepool-ghc-finalized-module-v1", homeUnits, finalizedModules]
```

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
for the same exact owner. Native receipt owners must match their finalized row
and have captured Core. `null` expressly has no source-free recovery input;
later native demand refuses instead of compiling from source.

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

`TPEXACTSCOPE8` always has nine fields: the seven existing declaration/interface/native fields, execution descriptor or null, and purpose authorization or null. Request authorization retains the strict `request-types2` wrapper and its explicit helper recipe. Each interface row has eight fields: the existing seven plus a typed role: `["value"]`, `["join"]`, or `["module", certificatePath, certificateSHA, corePathOrNull, coreSHAOrNull]` for a canonical module. The role comes from the admitted artifact kind; null and spelling-based classification are rejected. The two Core fields are both null or both present. Core is a compiler input; it carries no lexical selection or executable lease.

`TPHOMEOWNERS5` adds the exact finalized-module certificate SHA as its ninth field. A native original admitted to the inventory must have this binding and the matching validated carrier. Null is permitted only for standalone ownership encoding without canonical module custody; it cannot admit a native original. The issuer chooses the execution digest and module certificate binding before encoding once. Recovery decodes and validates both durable certificates and all selected bytes.

The existing `TPMCAN10` record references the same captured canonical certificate and Core through `RecoveryModuleInterfaceRef`. Publication completes those captured files before exposing the record. Candidate selection validates the configured producer, source digest, exact native owner, canonical certificate and payloads; the candidate retains the resulting immutable typed carrier. There is no alternate cache for type-only modules.

The inventory retains immutable variants by content ID. Each selected sealed closure has one canonical interface ID per exact owner and an exact native child index keyed by unit, module, version, interface SHA and product SHA. Admission and merge validate that selected closure before mutation; unrelated retained views cannot supply dependencies or conflict with its owners. Native children point to the exact canonical interface carried by their product. Interface projections select canonical IDs and cannot reach native children. A later native admission cannot alter an earlier view's outgoing edges or broaden its roots. Multiple native implementations may coexist only when their canonical carrier ID agrees; compilation materialization refuses an ambiguous native owner because the request wire selects one implementation per owner.

The worker candidate manifest is strict `TPMCAN10`: seven outer fields (the existing six plus the configured canonical producer SHA), with sixteen fields per candidate. Fields 14 and 15 retain the exact canonical requirement keys and `["module", certificatePath, certificateSHA, corePath, coreSHA]`. Native candidates require Core. Selection repeatedly removes candidates whose sealed dependencies are absent or differ from the actual exact-view interfaces and surviving candidates, recording a typed refusal; it never derives interface authority from source imports.

Deployment catalogs use strict schema 3. Every native module row retains its canonical interface certificate and Core reference through the existing recovery-file owner, alongside its native ownership seal. Export refuses missing canonical custody; cold loading validates both certificates, the exact full native owner, producer/source binding, and payload checksums before offering a candidate. Canonical payloads charge the existing aggregate budget. Catalogs from schemas 1 and 2 require regeneration through the deployment producer.
