# tidepool-codegen — prepared-STG native execution

This crate validates prepared execution programs, lowers them to Cranelift, and owns the heap, collector integration, continuation ledger, and prepared machine. GHC's internal Core stages are upstream compiler implementation details; this crate has no Tidepool Core IR or Core execution backend.

## Cost diagnostics

`TIDEPOOL_CODEGEN_DETAIL=1` adds native category and top-binding-inclusive
defining-module counts, blocks, emitted bytes, and native compilation time to
the `tidepool_codegen::prepared_compile` trace. Category and definition rows
carry distinct `metric_scope` fields and are emitted only for successful
compiles. It also reports dispatcher demands before and after expansion and
actual offers.

`TIDEPOOL_MEMORY_DETAIL=1` reports cumulative lifetime permanent-literal
storage and the physical-storage delta admitted by each installation. It also
reports lifetime static-region lookup calls, probes, hits, and maximum region
count when the collector or observation view drops. These counters measure
work; they do not change membership, ownership, or reclamation.
