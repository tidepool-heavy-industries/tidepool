# tidepool-extract-cmd — the ONE `tidepool-extract` invocation builder

**Charter.** Belongs: binary resolution (the strict `$TIDEPOOL_EXTRACT`
policy), typed argument construction (`ExtractCmd`), and the spawn + spawn
counter — a std-only leaf with zero deps. Does NOT belong: parsing extract's
output (diagnostics JSON, CBOR payloads — each caller maps `Output` to its
own error type), the compile memo/cache (`tidepool-runtime::cache`, which
consumes this crate's `argv()` as its cache key).
