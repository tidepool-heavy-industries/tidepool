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

## GHC execution-stack snapshots in prepared programs

The `800ab0d06` corpus had 103 target closures whose first projection blocker was
`__primcall ghc-internal stg_cloneMyStackzh`, with `[VoidRep]` arguments and
`[UnliftedRefRep]` results. Each also lacks exact recovered bodies for
`GHC.Internal.ExecutionStack.Internal.stackFrames` and
`GHC.Internal.Stack.CCS.$wgo`. These are shared dependency-closure blockers,
not evidence that each target executes a snapshot.

In [pinned GHC 9.12.2's backtrace collector](https://downloads.haskell.org/ghc/9.12.2/docs/libraries/ghc-internal-9.1202.0-a87f/src/GHC.Internal.Exception.Backtrace.html),
the clone-and-decode call is guarded by `IPEBacktrace`; its default is off,
but `setBacktraceMechanismState` can change it. The separate execution-stack
and cost-centre paths account for the other missing bodies. The
[clone primitive](https://downloads.haskell.org/ghc/9.12.2/docs/libraries/ghc-internal-9.1202.0-a87f/src/GHC.Internal.Stack.CloneStack.html)
produces a GC-managed copy of the active thread's stack. Prepared `raise#`
retains its exception operand as a machine root and records a terminal cause;
that is not stack capture or IPE decoding. Neither an empty snapshot nor a
default-off specialization preserves the mutable backtrace contract. Supporting
this path requires an explicit stack snapshot, ownership, and decoding design.
The delivery catalog instead preserves the pinned call signatures and reports
typed UnsupportedCapability if execution reaches a catalogued stack boundary.
It does not implement snapshots or claim that every enclosing program succeeds.

## Wired-in error boundary

The `patError` global has an exact fat-interface RHS but no serialized
authentic defining binder type. GHC 9.12.2 deliberately omits wired-in names
from ordinary interface declarations; an external extra-declaration binder
stores its name, not its type or IdInfo. Deserialization resolves that name to
the wired-in representation-polymorphic error Id, while the serialized source
RHS has a lifted result. Installing a different lookup environment cannot
retrieve information that was never written.

See the pinned [interface writer](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/Iface/Make.hs),
[external binder encoding](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/CoreToIface.hs),
and [wired-in resolution](https://github.com/ghc/ghc/blob/ghc-9.12.2-release/compiler/GHC/IfaceToCore.hs).
The prepared `NoSuccess` contract removes result representations at the
execution boundary; it does not make a mismatched Core binder/RHS well typed.

The delivery path recognizes GHC's wired-in keys and synthesizes ordinary
callable tops containing a typed WiredInError operation. It does not recover
the incompatible Core body or guess a replacement binder type. Native lowering
records the bounded decoded message directly. The remaining integration work is
session presentation of raised exception operands; the wired-in path already
retains its message without requiring exception-heap observation.
