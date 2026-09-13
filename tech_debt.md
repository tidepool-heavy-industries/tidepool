# Technical debt

## Declarative prepared-STG wire codec

The prepared-STG boundary uses positional CBOR records and tagged arrays.
Rust's decoder manually matches tags, lengths, and field indices in
`tidepool-repr/src/execution_schema/codec.rs`; Haskell independently encodes
the same layout in `haskell/src/Tidepool/ExecutionEncode.hs`. The schema types
are typed, but the wire layout is duplicated procedural code. A previous
GlobalDecl field-order mismatch demonstrated the maintenance risk.

Deferred until after the current STG work: evaluate declarative CBOR codecs
(for example, minicbor derives) or a shared schema that generates both sides.
For arbitrary binary layouts, binrw is another candidate, though this boundary
already uses CBOR. Rust derives alone would reduce decoder boilerplate without
eliminating Haskell/Rust schema drift; assess that distinction explicitly.

Preserve the flat expression arena, stack-safe decoding, bounded resource use,
typed malformed-input failures, explicit version rejection, and real
Haskell-produced fixture tests. Check candidate handling of unknown fields,
missing fields, enum layout, and allocation limits before choosing it. Any
wire-layout change requires an intentional coordinated migration.

Wave 5 continues with the existing codec; this entry does not authorize a
serialization redesign during that wave.

## Prepared formatter allocation and source-less authority

The native `Tidepool.Double` formatter calls `haskell_show_double` and may
build a second `String` for precedence parentheses before it allocates the
checked external byte payload. Those temporary Rust `String` allocations are
not fallible, so process OOM can abort instead of yielding the prepared
runtime's typed `HeapOverflow`. The external payload allocation and store
already have checked failure paths; this debt is specific to temporary
formatting storage, not all Text allocation.

Formatter replacement is deliberately limited to loaded source whose bytes
equal the extractor's compiled-in shipped `Tidepool/Double.hs`. A source-less
package interface cannot prove that identity and stays on ordinary recovery.
There is also no GHC stack-snapshot intrinsic in the prepared operation
catalog; internal root snapshots are not a substitute for one. These are
scope boundaries, not grounds to trust module spelling or invent an intrinsic.
