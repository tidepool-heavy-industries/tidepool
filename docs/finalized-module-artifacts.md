# Canonical finalized module artifact contract

One GHC finalization owns the canonical skinny interface and its matching tidy
Core. Preparation derives native products from that result. Checked-only HPT
interfaces are provisional and cannot issue this certificate.

## Worker receipt

`TPCERT6` is exactly seven CBOR fields:

```
["TPCERT", 6, nativeModules, targets, packages, globalDictionary, envelope]
envelope = ["tidepool-ghc-finalized-module-v1", homeUnits, finalizedModules]
```

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
individually limited to32 MiB, package witnesses to4 MiB; receipt metadata is4 MiB,
128 modules/home units, and aggregate captured finalization payload is128 MiB.

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
