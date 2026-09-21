# tidepool-codegen — prepared-STG native execution

This crate validates prepared execution programs, lowers them to Cranelift, and owns the heap, collector integration, continuation ledger, and prepared machine. GHC's internal Core stages are upstream compiler implementation details; this crate has no Tidepool Core IR or Core execution backend.
