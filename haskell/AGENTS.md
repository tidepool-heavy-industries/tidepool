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

## Resident API contracts

- Effect membership expresses callable intent, not resource authority. Opaque
  handles and Rust interpreters enforce ownership; inherited bindings do not
  transfer their author's permissions.
- Keep pure observations distinct from effects: `inspectFull` constructs a
  presentation value. Bind an effect result before inspecting it, or use
  `inspectFull <$> action`; do not disguise inspection as an effect.
- Actor orchestration surface is also generated/composed by `tidepool-actor`.
  Search its production consumers before adding a public Haskell helper.
- Preserve the distinction between actor `Watch result` and event `EventWatch`.
  Do not resolve public-name collisions with import hiding or duplicate aliases.
- Verify changed Haskell consumers in the repository Nix/toolchain environment.
  Use `just fixtures-check` after extractor translation or serialization changes.
