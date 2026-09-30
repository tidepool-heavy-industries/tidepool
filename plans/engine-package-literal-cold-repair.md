# Certified package literal cold-start repair

The failed cold successor selected a legitimate cached original group whose
foreign GHC string literals were represented as Address globals. Native
reference-handle admission refused the group before installation. The repair
specializes only exact package Bytes definitions from the same protected target
into existing image-owned literal storage. It preserves the original certified
group, GlobalIds, body, import inventory and serialized products.

## Failure and exact product inspection

The retained production recovery pair failed on its second cold host in
`installTools`, before manifest publication, after 492.169 seconds. The first
host succeeded. Source `f3b2` selected `main:Tidepool.Aeson.Value`, original
ordinal 183. The typed diagnostic identified GlobalId 9:

```
ghc-internal:GHC.Internal.Show:value:$fShowBool2
rep = Address
entry_signature = None
required_evaluated = true
required_generation = None
```

This was whole-group global admission, rather than an already published native
root, TPCERT3 decoding error or missing managed handle.

The exact cached record is:

```
/home/inanna/.cache/tidepool/module-candidates-v6/
6bb3c48d28d3b48bb85375aefdad12d774109f9444abad450ac3234ef62b8855/
c2727c81f50fb6da7768064ce4ab23232dc7b68b37ad61e0a6e454c2c40b0892.cbor
```

Its product SHA is
`4e096b844e66a65c40af92d871bcf3e055ca87233d7aa4dd516cbe27dd721cbd`
and interface SHA is
`1f10561e0ab415804541dc03e34d7d14bc9d8324276c2797c7ea73cf15d6135f`,
matching the failed diagnostic owner. Group 183 has 11 globals, 248 expression
nodes and six top bindings. Address globals are exactly IDs 9 and 10, package
`GHC.Internal.Show` binders `$fShowBool2` and `$fShowBool1`. Each occurs in four
body atoms; neither is referenced by a top-level function capture or constructor
field. The extracted neutral group SHA is
`b93f22a3c9183237a9252f4cfa9b0816727e2356919ed2f3db8d6fa8f0918e80`.

`ExecutionProjection` projects each original home group separately, which
externalizes those package binders. The recovered full target includes the
actual `HeapRhs::Bytes` definitions. Therefore the existing protected package
interface and same-target proof can select those definitions without changing
the original product or recovering source again.

## Ownership and admission

Native implementation: `9d4dcda90cebbe062c3a9ccb194c5c28e58a3280`.
Selected capture and focused test follow-up: `ae9152e24`.
Protected runtime consumer: `97ddd6c1f47dddde17c2ebb809288f0ec4af7bc3`.

`CompiledProgram::package_literals` inventories actual compiled Bytes tops.
Its private-field `PackageLiteral` retains full identity, interface facts and
the actual immutable byte allocation, including GHC's final NUL. This native
storage capsule does not issue package authority: the runtime first verifies
`CertifiedTargetPackageInterfaces::matches_target` against the same prepared
target and obtains the exact nonzero interface digest from that protected owner.
An unproved target supplies an empty inventory.

`DemandedImage::compile_with_package_literals` selects only Address globals
with an exact Package import owner, full identity/unit/module, identical
nonzero interface digest, required evaluated state, no entry signature and no
generation. Generic compilation remains reference-only. Home/Source Address
imports, arbitrary scalar imports and unmanaged pointer handles are refused.

The existing ImageRegistry key includes the original neutral group plus the
complete selected GlobalId/literal contract and bytes. Exact full-content
equality remains the collision fallback. Only selected literals specialize a
group key; unrelated target literals do not fragment it.

The existing PinnedBytes/image owner pins storage for dynamic slot reads and
static constructor/function captures. Selected literal slots are initialized
internally and excluded from managed root registration. Batch Source admission
also compares the actual selected top's identity, representation, signature,
evaluated state and full byte content before mutation. Existing handles cannot
supply those slots. Parcel extraction excludes internal literal slots; the
parcel retains the image, and receiver installation restores its internal
literal addresses. No managed CodeExport, binding ID, public handle or source
lease is issued for Bytes.

Selected-only Global function captures keep their exact global slot lookup and
static image storage. Generic Global captures retain their prior refusal.
Prepared `eqAddr#` remains unsupported: this parcel does not introduce pointer
equality semantics or rewrite equality by content. Any later admission of
address pointer equality requires a separate physical allocation identity
contract. Existing machine literal pools retain admitted allocations until the
machine drops, including after the producing program retires.

## Executed checks and limits

Pinned Rust 1.93.0/GHC 9.12.2, dedicated target
`/tmp/tidepool-binding-native-target`, jobs 6, one active owning compile lane:

```sh
systemd-run --user --unit=tidepool-native-package-literal-tests-10 \
  --slice=tidepool-completion-build.slice \
  --working-directory=/tmp/tidepool-wave-binding-native \
  --collect --wait --pipe bash scripts/dev-shell.sh bash -c \
  'rustfmt --edition 2021 tidepool/codegen/src/prepared_program/package_literals.rs \
    tidepool/codegen/src/prepared_program/package_literals/tests.rs && \
   CARGO_TARGET_DIR=/tmp/tidepool-binding-native-target CARGO_BUILD_JOBS=6 \
   cargo test -p tidepool-codegen --lib prepared_program::package_literals::tests:: \
     -- --test-threads=1 --nocapture'
```

Eight tests passed, 531 filtered out, 0.03 seconds execution. Service runtime
10.898 seconds includes incremental compilation; peak 690.9 MiB. The tests
cover dynamic and static captures, static constructor Address fields, actual
untraced slot registration, target/program retirement, full-content image reuse
and changed bytes, interface/evaluated-state refusals, Source Address and generic
Global capture refusal, different Source bytes and managed Source/Existing
handle rejection before mutation, cross-machine parcel lifetime after sender
drop, final managed program reclamation, and `eqAddr#` refusal. The prior generic
float-global diagnostic gate passed one test, 530 filtered out.

Evidence is retained under
`/tmp/tidepool-wave-binding-native/target/completion-evidence/binding-native/`:
`package-literal-api02.log`, `package-literal-tests10.log`,
`actual-cold-group183.cbor`, `actual-cold-group183.json` and the bounded read-only
decoder `inspect-cold-group.py`. Earlier failed compile/fixture attempts remain
in numbered logs; they are not passing evidence. Formatting and `git diff
--check` passed. The runtime owner separately reports four focused runtime tests
and runtime/actor target compilation.

These checks establish the owning native contracts. The joined production cold
recovery rerun is a separate acceptance gate and was still pending when this
report was frozen. No end-to-end latency improvement or scaling regression is
claimed. Literal specialization adds exact byte/contract key work; persistent
binding metadata, compiler input writes, checked-prefix compilation and full
runtime scaling remain separate measured parcels. Buck source registration and
local-only packaging verification belong to the final build owner.
