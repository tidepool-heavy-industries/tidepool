# tidepool-guess — number-guessing game demo

**Charter.** Belongs: an end-to-end Tidepool workflow demo (Haskell via
`haskell_inline!`, JIT via Cranelift, hand-rolled `Console`/`Rng` effect
handlers driving real terminal IO). Does NOT belong: production effect
handlers — this example deliberately does not reuse
`tidepool_handlers::build_base_stack` because its synchronous-stdin needs
don't fit that stack's shape.
