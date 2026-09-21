# tidepool-codegen — prepared-STG native execution

This crate validates prepared execution programs, lowers them to Cranelift, and owns the heap, collector integration, continuation ledger, and prepared machine. GHC's internal Core stages are upstream compiler implementation details; this crate has no Tidepool Core IR or Core execution backend.

## Cost diagnostics

`TIDEPOOL_CODEGEN_DETAIL=1` adds native category and defining-module counts,
blocks, and emitted bytes to the `tidepool_codegen::prepared_compile` trace.
It also reports dispatcher demands before and after expansion and actual
offers. Local closures are explicitly unattributed to a defining module.

`TIDEPOOL_MEMORY_DETAIL=1` reports permanent literal storage at installation
and static-region lookup calls, probes, hits, and maximum region count when
the collector or observation view drops. These counters measure work; they
do not change membership, ownership, or reclamation.
