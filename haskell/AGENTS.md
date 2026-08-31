# Haskell extractor and DSL

This directory owns the GHC-to-Core extractor worker and the model-facing
`Tidepool` library. Rust owns CLI parsing, daemon/process lifecycle, toolchain
discovery, cache policy, and artifact decoding.

- `app/Main.hs` decodes typed worker requests and dispatches compiler work. It
  is not a user-facing CLI or a workflow engine.
- `Tidepool.WorkerServer` owns framing; `Tidepool.GhcPipeline` owns resident
  compiler state. Keep request-local modules out of the reusable compiler memo.
- Put reusable pure DSL helpers in the narrowest named `lib/Tidepool` module,
  export them explicitly, and auto-import them only when they belong in the
  default model vocabulary.
- Prefer familiar Haskell interfaces. Use `Member` constraints and avoid
  exposing concrete row order. Effect constructors and wire records originate
  in `tidepool-protocol`/`tidepool-mcp`, not handwritten duplicates here.
- Do not preserve persisted values by inventing syntactic bans. Let GHC check
  whether a value can be used in the receiving effect row.
- After translation or serialization changes, run the canonical fixture check;
  never hand-edit or selectively omit generated CBOR artifacts.
- Do not share `dist-newstyle` between worktrees. Use Nix plus a worktree-local
  Cabal build for focused extractor tests.
