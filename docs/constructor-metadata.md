# Constructor metadata contract

GHC issues complete nominal identities from the exact defining interface owner.
General `SymbolIdentity` retains unit, module, namespace, occurrence and record
parent. `TypeIdentity { symbol, version }` adds mandatory declaration evidence:
compiler-known `WiredIn` or a 128-bit GHC declaration ABI fingerprint.
`ConstructorIdentity { symbol, family: TypeIdentity }` carries the defining
family revision. Constructor declarations derive their family from that identity;
there is no second family field. Promoted type names preserve their own spelling
and use the defining parent declaration ABI.

`DCMeta.dcmIdentity` and `DataCon.identity` retain the same complete constructor
identity. Diagnostic aliases and rendered type names remain lookup hints. A
spelling with several complete identities is ambiguous even when their arities
differ. Exact identity lookup selects authority; shape checks cannot choose a
unit or type revision. Numeric ID and shape conflicts refuse before replacement.

The canonical CBOR grammar uses definite arrays and shortest primitive encodings:
`symbol = [unit, module, namespace, occurrence, [0] | [1, recordParent]]`,
`version = [0] | [1, bytes16]`, `type = [symbol, version]`, and
`constructor = [symbol, type]`. Fingerprints encode two big-endian Word64s as one
16-byte string. Missing, extra, unknown and wrong-width evidence refuses; no
rendered fingerprint, optional version or compatibility default exists.

The current `TPLR` header is version 6.0. Each metadata row has ten fields:
ID, occurrence, tag, representation arity, bangs, qualified alias, field labels,
rendered parent type, rendered field types, and complete constructor identity.
Both standalone metadata decoding and table admission validate constructor and
family symbols. Regenerate internal artifacts from an authorized producer.

| Producer / consumer | Current format | Migration and admission |
| --- | --- | --- |
| Haskell `ExecutionEncode` / repr prepared codec | TPSTG 19, execution ABI 10 | Previously 18 / 10; constructor-scheme roots retain their complete telescope and body without root binder-kind annotations; declaration and explicit forall kinds remain |
| Haskell `CborEncode` / repr `serial::{read,write}` | TPLR 6.0 | Previously 5.0; tenth field now carries complete constructor identity; old headers refuse |
| Haskell `ExecutionEncode` / repr projected-group codec | TPGRP 2 | Previously 1; embedded declaration and type grammar changes; old group headers refuse |
| Haskell `ExecutionEncode` / repr finite type-graph codec | TypeGraph 2 | Previously 1; declaration identities retain versions; old graph headers refuse |
| Haskell `ExecutionEncode` / repr module-product reader | TPMOD 1 | Outer grammar contains opaque group bytes; inner TPGRP 2 and prepared schema 19 admission rejects obsolete groups |
| Repr type graph / enclosing commitment owners | Evidence/content domain v2 | Previously v1; explicit framing includes version tag, ABI bytes and constructor family identity |
| `NativeOriginalCensus` / native original reader | TPHOMEOWNERS 5 unchanged | Binder/global identities are term-only SymbolIdentity; embedded prepared/metadata bytes retain strict inner admission |
| Module candidates / candidate reader | TPMCAN 10 unchanged | Candidate binders/global identities are term-only; execution graph descriptors are opaque |
| Compiler cache recipes | Exact producer identity | Producer changes with the emitter; readers independently reject obsolete inner payloads |
| Durable JSONL and independent protocols | Unchanged | No direct constructor metadata grammar coupling |

`ExecutionProjection.internConstructor` issues a declaration and retains its real
GHC `DataCon` together. The production worker finalizes the requested target and
retained original products before emitting sidecars. The corpus producer gathers
constructors from every successfully emitted target, including all-top targets,
before encoding its shared table. Wired-in rows also use actual GHC owners.

Before completion, producers may collect programs and metadata separately. At
joint admission the table must include every declared constructor, including
unused declarations, external constructors and Typeable settlement constructors.
No absence tolerance applies. Extra complete rows are permitted in a shared
multi-target table. A program with no constructor declarations needs no rows.

The repr-owned paired reader and `DataConTable::validate_program` enforce this
agreement at assembly, checked item and preview sealing, native output decoding,
retained entry reload and corpus admission. Runtime `CompiledTurn` keeps its
program and table private; its ordinary constructor checks the same complete
pair, and the native owner attaches checked certification separately. `TurnCode`
preserves the private pair through borrowing and owned transfer. Installation
consumes the program while retaining borrowed metadata and certification; it
does not clone unused site inventories. Ordinary multi-target installation
revalidates its selected pair before machine admission.

The compiler is the sole numeric constructor-ID issuer. For non-wired-in
constructors it hashes `tidepool-constructor-id/v1\0` followed by canonical
constructor-identity CBOR with SHA-256, takes the first seven digest bytes in
big-endian order, and retains the reserved `0xFE` high-byte tag. Matched compiler
wired-in constructors retain their existing host primitive ABI, based on actual
GHC wired-in evidence. Reserved array, closure and oversize sentinels remain
unchanged. Full typed identity remains authoritative despite truncation;
collisions refuse before mutation. ABI fingerprints discriminate declarations;
they do not replace original provenance or representation checks.

Generated runtime fixtures are rebuilt from their owning sources and targets.
Independent protocol and durable-format goldens keep their own contracts.
