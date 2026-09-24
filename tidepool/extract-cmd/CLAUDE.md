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

`ExtractCmd::bind()` resolves one opaque compiler endpoint before callers use
its identity. A daemon is preflighted and epoch-bound; an unavailable daemon
before submission binds direct. `CompilerEndpoint::execute()` is the sole
library execution route. An epoch/stamp rejection is known unsubmitted and may
be rebound and re-keyed; after acceptance, a lost response is indeterminate
and is never replayed.

Ordinary daemon mode exits when its request or RSS rotation bound is reached.
`--persistent` keeps the daemon endpoint alive and rotates only the pinned GHC
worker, for a long-lived owner such as one Exomonad tmux session.
An explicit compiler transaction pins that worker across its ordered requests;
rotation and request-local compiler cleanup occur when the transaction closes.
Every request against the pinned worker (plain and transaction-pinned alike)
is bounded by an absolute request deadline (`--request-deadline-secs`,
default 15 minutes); a worker that never replies is killed at expiry and the
daemon recovers it through the same worker-replacement path a crash uses,
which only `--persistent` survives.

A `--persistent` daemon serves `--workers N` concurrent GHC workers
(`daemon::DEFAULT_WORKER_COUNT`, 3 by default) rather than one: a single
accept thread still owns every fence check (epoch, watched-stamp) and
PREFLIGHT/STOP handling, but hands each accepted, fenced connection to a free
worker slot over a bounded (rendezvous) queue — an over-subscribed daemon
backs up in the kernel's own listen backlog, never in an unbounded set of
spawned threads. Each slot is a full pinned worker with its own transaction
pinning, request deadline, peer-disconnect kill, and served/RSS rotation;
rotation replaces a slot's worker in place, same as the single-worker case.
`STOP` and a watched-stamp change stop accepting, let every slot finish its
current job (bounded by that job's own request deadline), and only then
drain and reject whatever is left queued. Ordinary (non-`--persistent`)
daemon mode always runs one worker and ignores `--workers`: it retires the
whole endpoint, not just a slot, the first time any request's rotation bound
is reached, which only suits the single short-lived worker that mode was
designed around.

`--workers` and `--rss-ceiling-mb` keep their historical meanings (worker
count and a per-worker RSS ceiling) when given explicitly. Their *defaults*
are both derived together from a shared total memory budget
(`daemon::default_memory_budget_mb()`): `daemon::worker_sizing_from_budget`
picks as many `daemon::WARM_WORKER_MB` (7 GiB, measured) slots as the budget
supports, capped at `daemon::DEFAULT_WORKER_COUNT`, and sets each slot's
ceiling to `budget / worker_count`. Worker *count* is derived from the
budget, not fixed, precisely so a smaller budget produces fewer, still-warm
workers instead of more, undersized ones that rotate before they finish
loading — the ceiling a lower-than-`WARM_WORKER_MB` slot would get discards
the module memo the ceiling exists to protect on almost every request. When
even a single worker's ceiling (the whole budget) falls short of
`WARM_WORKER_MB`, the daemon still runs one worker at that ceiling (the
existing floor behaviour) and logs a WARN that it cannot keep a worker warm;
an explicit `--workers` that produces the same shortfall gets the same WARN.

That total budget is itself the smaller of a measured fixed ceiling and
memory actually available at daemon start (`/proc/meminfo`'s
`MemAvailable`, minus a headroom reserve): a quiet box still gets the fixed
figure, but a daemon that starts next to another compiler daemon already
holding RSS — this repo's own persistent test daemon, or an unrelated
caller's — sizes down instead of assuming the whole machine budget is free.
This is deliberately the only place daemon sizing reads machine memory; it
does not register anywhere or coordinate with the other daemon directly, and
adds no second budget registry alongside `exomonad-node`'s
`command_resources` admission service, which already gates actor starts on
the same `MemAvailable` figure. The daemon logs its derived sizing
(`available_mb`, `headroom_mb`, `budget_mb`, `workers`, `rss_ceiling_mb`,
`warm_worker_mb`) once at startup, and a worker replaced by RSS rotation
after serving only a few requests logs as memo loss, not ordinary rotation.
See `daemon::DEFAULT_WORKER_COUNT`, `daemon::WARM_WORKER_MB`, and
`daemon::default_memory_budget_mb`'s doc comments for the exact sizing and
the matching `.config/nextest.toml` `[test-groups.ghc-heavy] max-threads`.

A pooled (`--workers` > 1) slot that has not yet served a real request
pre-warms itself once it has gone confirmed-idle for a short debounce and
some other slot's first real request has revealed an include set (source
root and argv): it runs one compile against that same include set on its
own worker, so its own first real request need not pay the cold
`ghc_load` + `lowering` cost the daemon's sizing exists to avoid repeating.
The real request's argv is never replayed verbatim — it names output paths
and can carry a session bind, both of which would touch the real request's
own files or shared session state. `ExtractRequest::redirect_outputs_for_warm_up`
derives a side-effect-free copy first: same includes/session root/target/
files, but every output-path field redirected into a scratch directory
removed immediately after, and the `--bind-gen` field dropped so a would-be
session bind fails a diagnostic rather than writing under session root. A
failed pre-warm only logs a WARN and respawns that slot's worker; it never
blocks the slot from serving normally. See `daemon::warm_up_slot`,
`daemon::should_attempt_warm_up`, and
`ExtractRequest::redirect_outputs_for_warm_up`'s doc comment.

The spawn counter counts logical extractor invocations, including requests
served by a resident worker. It is an observability API, not a process-fork
counter.

Every process edge in the extractor chain uses the crate's parent-death
contract. Killing a caller, frontend, or daemon must reap its frontend or GHC
worker descendants; process-tree ownership is not delegated to test scripts.

## Wire boundary

The crate remains a dependency leaf for proc macros. In addition to `blake3`
for immutable endpoint identity, it uses the workspace tracing stack because
the compiler CLI owns daemon process observability. Its wire formats are small,
versioned, and implemented in-repo. Keep framing and field validation here;
keep compiler interpretation in the Haskell worker.

When adding a request field:

1. add it to the typed Rust request;
2. update the versioned encoder and Haskell decoder together;
3. make invalid combinations unrepresentable in Rust where practical;
4. test the round trip and the worker behavior;
5. avoid adding a second textual protocol for the same operation.
