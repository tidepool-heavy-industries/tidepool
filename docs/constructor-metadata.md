# Constructor metadata contract

GHC issues a constructor's full `SymbolIdentity` from its actual `Name` owner:
unit, module, namespace, occurrence and record parent. `DCMeta.dcmIdentity` and
`DataCon.identity` carry that same representation. Diagnostic aliases remain
optional render and lookup hints. A spelling is usable only when its nominal
candidate is unique; module qualification alone cannot choose a unit.

The current `TPLR` header is version 5.0. Each metadata row has ten fields:
ID, occurrence, tag, representation arity, bangs, qualified alias, field labels,
rendered parent type, rendered field types, and the existing five-field symbol
grammar. Missing owners, invalid constructor namespaces, spelling disagreement,
and obsolete ambiguous versions are rejected. Regenerate internal artifacts from
an authorized producer; no unit reconstruction or compatibility adapter exists.

| Format | Migration | Reason |
| --- | --- | --- |
| TPLR constructor metadata | 4.0 to 5.0; old payloads rejected | Required complete symbol adds the tenth field |
| TPSTG prepared schema and execution ABI | 16 and 9 unchanged | Existing complete symbols and execution grammar are unchanged |
| TPGRP, module products, checked-cell and native receipts | Outer versions unchanged | Complete identity already exists or metadata bytes remain opaque; inner TPLR admission rejects obsolete metadata |
| Compiler cache recipes | Container unchanged | Exact producer identity changes with the emitter; the inner reader independently rejects stale metadata |
| Durable JSONL and independent protocols | Unchanged | No constructor metadata schema coupling |

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

The host ID hash is unchanged. Different full identities at one ID refuse before
replacement. One full identity cannot claim multiple IDs. Consequently this
migration preserves identity and refusal; it does not enable installing real
homonymous units that share an existing host ID.

Generated runtime fixtures are rebuilt from their owning sources and targets.
Independent protocol and durable-format goldens keep their own contracts.
