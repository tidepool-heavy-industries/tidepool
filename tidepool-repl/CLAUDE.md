# tidepool-repl — stateful GHCi-style MCP server

## Charter

This crate owns the resident-session MCP protocol, REPL command extensions,
presentation, and single-session manager. Frontend-neutral workbench
classification and sequencing live in `tidepool-runtime`; the JIT lives in
`tidepool-codegen`, and effect definitions live in `tidepool-mcp`.

## MCP surface

The server exposes:

- `session_run`: execute declarations, statements, expressions, or meta commands;
- `session_resume`: answer a suspended `ask`;
- `session_abort`: abandon a suspended request;
- a session resource describing current bindings and declarations.

Keep request/response details in the generated tool descriptions and protocol
types. Do not maintain a second JSON reference here.

## Block execution

A request may contain several top-level items. The block runner maps shared
workbench classifications into REPL operations:

- declarations extend the persistent declaration environment;
- bind statements create persistent heap bindings;
- expressions evaluate and update `it` where applicable;
- supported meta commands inspect or modify session state.

GHC remains authoritative for ambiguous Haskell. Do not add a second lexical
classifier or meta-command tokenizer in this crate. Multiline declarations,
comments, strings, and layout must survive intact.

Declarations successfully compiled before a later item fails remain committed.
A suspension records the cursor and materialization policy needed to resume the
same item and continue the rest of the block exactly once.

## Session lifecycle

The manager is a single-slot client of
`tidepool_runtime::session::registry::SingleSlot`.

- Every run checks out the session and settles it once as idle, suspended,
  retired, or wedged.
- Epochs fence late timeout or panic settlement from overwriting newer state.
- A suspended session is threadless: the continuation, pending item tail, and
  block cursor are data owned by the session.
- Only the matching suspended request may be resumed or aborted.
- A bottom-bearing resume answer does not consume the continuation.
- A wedged slot remains visible until reset or TTL reap; do not silently create
  a replacement session.

The REPL uses the JIT registry's capacity-one façade. The continuation remains
registry-rooted even though the REPL carries only one active request.

## Persistence semantics

Bindings, declarations, and heap live for the machine session. A reset or
retirement drops them. The REPL is not a durable database and must not imply
that state survives server restart.

Generated effects use stable `Tidepool.Effects.Core` plus a per-turn row shim.
Persistent declarations may be row-polymorphic; values whose types pin a
per-turn concrete row must not cross turn boundaries.

## Toolchain and configuration

Typical launcher configuration:

```json
{
  "command": "tidepool-repl"
}
```

Relevant environment variables are resolved by shared crates:

- `TIDEPOOL_EXTRACT`
- `TIDEPOOL_PRELUDE_DIR`
- `TIDEPOOL_TOOLCHAIN_STAMP`
- `TIDEPOOL_TOOLCHAIN_HANDSHAKE`
- `TIDEPOOL_CONFIG_DIR`
- `TIDEPOOL_EVAL_TIMEOUT_SECS`

Do not add REPL-specific copies of toolchain or path precedence.

## Caller guidance

- Use declarations for reusable functions and types, binds for reusable values,
  and expressions for results.
- A suspended `ask` must be resumed through `session_resume`, not by issuing a
  new block and guessing which continuation is active.
- Treat compile failures as ordinary session responses; they do not imply the
  server or session died.
- Large values may stay bound even when their rendered representation is
  abbreviated.

## Verification

Test classification, partial commit, bind/`it` behavior, suspension identity,
timeout settlement, reset/reap, and cross-turn declaration compatibility.

This crate is GHC-heavy:

```bash
scripts/battery.sh -p tidepool-repl -E 'test(<name>)'
```

Use the binary sub-shards in `scripts/battery-shard.sh` for broader coverage.
