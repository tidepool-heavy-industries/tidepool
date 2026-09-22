# Model-authored text parsing

This crate is the sole provider-neutral parser for explicitly marked regions
in model output, including fenced Haskell. It does not call providers, execute
code, own conversations, or decide runtime policy.

- Extend this parser instead of adding frontend-local fence extraction.
- Parsing must be deterministic, preserve source text exactly where promised,
  and distinguish malformed/incomplete regions explicitly.
- Keep provider quirks in provider adapters and execution/retry decisions in
  consumers. This crate identifies syntax; it does not assign authority.
- Cover delimiter, language-label, whitespace, multiple-block, incomplete,
  and adversarial text cases with small behavioral tests.
