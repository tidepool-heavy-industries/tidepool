# Resident GHCi-style frontend

This crate owns the MCP-facing resident REPL, command extensions,
presentation, and its single-session manager. Classification and sequencing
come from `tidepool-runtime::session::workbench`.

- GHC is authoritative for ambiguous Haskell. Do not add another lexical
  classifier or meta-command tokenizer.
- A multi-item block commits successful declarations before a later failure.
  Suspension retains the exact cursor and resumes the same item/tail once.
- Checkout and settle the single session exactly once. Epochs fence late
  timeout/panic settlement; wedged sessions stay visible until reset or reap.
- Only the matching request may resume or abort a suspension. A bottom-bearing
  answer does not consume its continuation.
- Persistent declarations may be row-polymorphic; values pinned to a per-turn
  concrete row must not cross turns.
- Use targeted GHC-backed tests through `scripts/battery.sh -p tidepool-repl
  -E 'test(<name>)'`; do not run broad shards during parallel work.
