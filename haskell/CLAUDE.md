# haskell/ — extractor and eval standard library

## Charter

This directory owns the GHC→Core extractor and `lib/Tidepool`, the Haskell
library auto-imported by the MCP surfaces. Toolchain discovery and caching live
in `tidepool-toolchain`; CBOR decoding lives in `tidepool-repr`.

## Build the compiler worker

From `haskell/`:

```bash
cabal build tidepool-extract-bin
cabal list-bin tidepool-extract-bin
```

The executable is an internal compiler worker, not a user-facing CLI. It accepts
only the versioned request protocol emitted by `tidepool-extract-cmd`, or
`--worker-loop-v1` when run behind the resident daemon. A plain `cabal build`
builds the worker; `cabal build --enable-tests` also builds the extractor test
components.

For local Rust tests, use the Rust frontend and point it at the worktree worker:

```bash
TIDEPOOL_EXTRACT=../target/debug/tidepool-extract \
TIDEPOOL_EXTRACT_WORKER=$(cabal list-bin tidepool-extract-bin) \
  PATH=<ghc-with-packages>/bin:$PATH \
  cargo nextest run --ignore-default-filter -p tidepool-runtime
```

Do not share `dist-newstyle` between worktrees. Nix already shares the compiler
and dependencies; `dist-newstyle` is mutable Cabal state.

## Toolchain resolution and deployment

`tidepool-toolchain/src/toolchain.rs` owns resolution and the startup
fingerprint check.

Frontend precedence:

1. `$TIDEPOOL_EXTRACT`; a set but invalid value is an error.
2. `tidepool-extract` on `PATH`.

Standard-library precedence:

1. `$TIDEPOOL_PRELUDE_DIR`; it must contain `Tidepool/Prelude.hs`.
2. A repository `haskell/lib` or `lib` found by walking upward from CWD.
3. The library beside a worktree-built extractor's `dist-newstyle`.
4. The library embedded in the server binary.
5. The source tree from which the binary was built.

Use `scripts/redeploy.sh` to deploy the extractor, Rust servers, embedded
library, cache state, and toolchain stamp as one operation. For deliberate
mixed local testing, set `TIDEPOOL_TOOLCHAIN_HANDSHAKE=warn`; do not weaken the
default handshake.

## Regenerate fixtures

After changing translation or serialization, build both halves and invoke the
Rust frontend:

```bash
cd haskell
WORKER=$(cabal list-bin tidepool-extract-bin)
cd ..
cargo build -p tidepool-extract-cmd --bin tidepool-extract
TIDEPOOL_EXTRACT_WORKER="$WORKER" target/debug/tidepool-extract \
  haskell/test/Suite.hs --all-closed \
  --include haskell/lib --target-module-only --output-dir haskell/test/suite_cbor
```

`--all-closed` treats its output directory as an owned fixture corpus: after a
successful write it removes stale `*.cbor` and `*.asks.json` artifacts from
earlier runs while preserving unrelated files. Current lifted-local
`*_t<n>.cbor` fixtures remain part of the differential corpus; do not manually
omit them. New CBOR fixtures are ignored by default and must be added
explicitly.

## Extractor diagnostics

Diagnostics are opt-in:

| Variable | Purpose |
|---|---|
| `TIDEPOOL_DUMP_CLOSED=<needle>` | print closed Core for matching bindings |
| `TIDEPOOL_VARID_AUDIT=1` | report VarId collisions |
| `TIDEPOOL_VARID_AUDIT=<hex>,...` | resolve selected VarIds to names |
| `TIDEPOOL_DANGLING_DEBUG=1` | show unresolved references before allowed session refs are removed |
| `TIDEPOOL_JOINREC_DEBUG=1` | trace join-rec translation |
| `TIDEPOOL_IFACE_DEBUG=1` | trace fat-interface loading |

`TIDEPOOL_TEST_DROP_DC` and `TIDEPOOL_TEST_FORCE_VALIDATION_ONLY` are
fault-injection controls for extractor tests, not debugging defaults.

## Worker lifetime

The Rust frontend owns CLI parsing, Unix sockets, daemon configuration, and
process lifecycle. It either starts this worker for one typed request or keeps
one worker alive with `--worker-loop-v1`. `Tidepool.WorkerServer` owns only the
framed stdin/stdout loop; `Tidepool.GhcPipeline` owns the resident compiler
state. `Main` decodes a typed request and dispatches compiler operations; it is
not a second CLI or workflow engine. Request-local target and
`Tidepool.Session.*` modules are removed from the shared memo after each
request; reusable library interfaces remain warm. Requests are serialized and
carry their own CWD and compiler options.

The worker process environment is fixed at startup. Restart the daemon after
changing extractor diagnostic variables, GHC configuration, or its watched
toolchain stamp.

## Eval library

`lib/Tidepool` is the model-facing Haskell surface. Prefer familiar Haskell
APIs and types; novelty here directly costs model fluency.

Key modules:

- `Tidepool.Prelude`: default imports and common helpers;
- `Tidepool.Form`: schema-derived operator forms;
- `Tidepool.Async`: authored concurrency;
- `Tidepool.Worktree`, `Shell`, `Cargo`: typed operational helpers;
- `Tidepool.Agent`: coding-agent delegation;
- generated `Tidepool.Effects`: the effect row and verbs for a compile.

Pure reusable helpers belong in named library modules. Effect constructors and
wire records belong in `tidepool-protocol`/`tidepool-mcp`, not handwritten
copies in the library.

`ask` and `llm` share the `Schema` vocabulary. `ask` suspends for a caller
reply; `llm` performs a server-side structured model call. Both validate the
returned JSON value against the schema.

## Adding Prelude or library functions

1. Put the implementation in the narrowest appropriate module.
2. Export it explicitly.
3. Add it to auto-imports only when it belongs in the default model-facing
   vocabulary.
4. Extend an existing bundled surface test where possible; do not create a new
   extractor compile for a one-line assertion.
5. Rebuild the extractor if the module is part of the deployed library.

## Current limits

- Topological recovery can only recover bindings whose dependencies are
  available in extracted Core or accepted session modules.
- Session-generated modules require the stable `Tidepool.Effects.Core` plus a
  per-compile row shim; do not persist types that pin a concrete effect row.
- The resident worker is single-threaded and changes process CWD per request;
  parallel request execution would require a different isolation model.

Add a limit here only when it is present, user-visible, and not already made
unrepresentable by the current API.
