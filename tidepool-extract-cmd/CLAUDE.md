# tidepool-extract-cmd — compiler-worker boundary

## Charter

This crate is the public process boundary for extraction. It owns:

- the human-facing CLI;
- strict extractor binary resolution;
- typed request construction and encoding;
- direct and resident-worker transport;
- process lifecycle and invocation accounting.

The Haskell executable behind this crate is a compiler worker. It receives a
versioned domain request, runs GHC, and returns compiler artifacts or
diagnostics. Do not put command-line grammar, JSON control planes, daemon
policy, or caller workflow orchestration in the worker.

Artifact decoding belongs to callers. Content-addressed compilation caching
and toolchain validation belong to `tidepool-toolchain`.

## Execution contract

`ExtractCmd::run()` uses the resident daemon when
`$TIDEPOOL_EXTRACT_DAEMON_SOCKET` is set and falls back to a direct worker
spawn only when connection failure proves the request was never submitted.
After connection, a lost or late response is indeterminate: the daemon may
still be compiling, so the error is returned instead of duplicating the
request through a direct worker.

`run_with(&Launcher::Daemon(path))` is different by design: the caller chose a
specific transport, so daemon failure is returned rather than hidden behind a
fallback.

The spawn counter counts logical extractor invocations, including requests
served by a resident worker. It is an observability API, not a process-fork
counter.

Every process edge in the extractor chain uses the crate's parent-death
contract. Killing a caller, frontend, or daemon must reap its frontend or GHC
worker descendants; process-tree ownership is not delegated to test scripts.

## Wire boundary

The crate is a std-only leaf because proc-macro crates depend on it. Its wire
formats are small, versioned, and implemented in-repo. Keep framing and field
validation here; keep compiler interpretation in the Haskell worker.

When adding a request field:

1. add it to the typed Rust request;
2. update the versioned encoder and Haskell decoder together;
3. make invalid combinations unrepresentable in Rust where practical;
4. test the round trip and the worker behavior;
5. avoid adding a second textual protocol for the same operation.
