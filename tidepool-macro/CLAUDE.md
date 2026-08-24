# tidepool-macro — `haskell_eval!`/`haskell_inline!` proc-macros

**Charter.** Belongs: embedding Haskell source as CBOR at build time —
invoking `tidepool-extract` at macro-expansion time (via
`tidepool-extract-cmd`) and splicing the resulting CBOR into the caller's
binary. Does NOT belong: the extract invocation builder itself
(`tidepool-extract-cmd`, this crate's own dependency), any runtime compile
path (`tidepool-runtime`).
