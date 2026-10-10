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
Scoped daemon binding retains its preflighted socket, boot epoch and producer
without reserving a worker or CPU grant. The first execution admits the exact
captured endpoint; stale epochs refuse rather than silently rebinding an already
prepared offer. Direct identity handshakes retain their existing child owner.
An explicit compiler transaction pins that worker across its ordered requests;
rotation and request-local compiler cleanup occur when the transaction closes.
An ordinary one-shot response is delivered only after the worker acknowledges
transaction cleanup, so a client closing after receiving it preserves the warm
worker. Explicit transactions stream request responses and acknowledge cleanup
separately when the client closes the transaction.
Request-owned input descriptors stay alive through that acknowledgement. Scoped
callers register the original descriptor with the transaction owner; uncertain
settlement carries physical custody in the retained close evidence. A request
response, cancelled waiter or missing scope does not establish input release.
Every request against the pinned worker (plain and transaction-pinned alike),
and each worker transaction begin/close acknowledgement, is bounded independently
by the configured request deadline (`--request-deadline-secs`,
default 15 minutes); a worker that never replies is killed at expiry and the
daemon recovers it through the same worker-replacement path a crash uses,
which only `--persistent` survives.

Host-side preparation polls `compiler_host_checkpoint` within the same compiler
scope. It returns `io::ErrorKind::Interrupted` when that scope is cancelled;
unscoped work is allowed. Polling acquires no endpoint or admission and leaves
accepted-request and END cleanup custody with the original owner.

Direct identity and transaction-BEGIN handshakes share the same process owner.
A scoped cancellation token arms the child immediately after spawn, before
identity reads; explicitly bound transactions arm it before BEGIN. Each
handshake has a five-minute absolute deadline and readiness-polled reads, so
partial replies do not renew the bound. Identity refusal remains known
unsubmitted; BEGIN ambiguity remains indeterminate. Refusal, cancellation and
timeout settle through the endpoint's existing child termination/reap owner.

A persistent daemon runs the existing pool of pinned GHC workers. One accept
owner checks epoch/deployment fences and routes through bounded per-slot queues.
The lowest live slot retains foreground context; preparation never occupies it.
Foreground uses that slot first and may spill to an idle additional slot. At most
one foreground job waits beyond occupied workers. Every accepted connection holds
its CPU grant through transaction cleanup and any owned worker rotation;
`compiler_capacity_release` records the exact admission after its permit is
released, including failed delivery, cancellation and replacement. Failed
acknowledgement, disconnect, worker failure and cancellation release the grant
through the same owned permit.
STOP and deployment changes drain accepted work before rejecting the backlog.

Requests and transaction acquisition declare `CompileWorkload::Foreground` or
`Preparation`. Existing calls default to foreground. Required root installation
is foreground; optional proactive preparation declares preparation before BEGIN.
The versioned worker envelope carries positive jobs and runtime capabilities; the
daemon replaces caller values with its admitted grant. Grants stay fixed across
an accepted transaction. Nested default transaction helpers join an explicit
outer transaction and retain its workload. Workload is urgency, not artifact
identity. Explicit classification belongs inside the actual blocking compiler
closure rather than depending on implicit propagation across async tasks.

The `--workers` setting controls the number of resident GHC worker processes;
foreground job width controls module scheduling and the worker executor within
each process. They are separate limits. Defaults allow two foreground jobs and
four preparation jobs per request, bounded by aggregate effective CPU
availability. `--foreground-jobs` and `--preparation-jobs` select positive
maxima for the qualification matrix. The Exomonad `[compiler]` configuration
can set `foreground_jobs`; omission preserves the daemon's default of two.
Changing job width does not add worker processes, but can increase one worker's
RSS and make its existing rotation ceiling arrive sooner. The accept owner
observes ancestor cgroup quotas/cpusets, ancestor memory remaining and host
memory headroom before acceptance. Preparation cannot borrow the reserved
foreground CPU allowance or warm worker. These defaults require measured latency
qualification; available CPU count alone does not establish a passing allocation.
Explicit `max` and a controller file absent from an existing cgroup impose no
additional memory limit. Unreadable or malformed installed memory limits, missing cgroup
directories, and unusable usage evidence under a finite limit establish zero
headroom; they cannot substitute host availability for the enclosing constraint.

A daemon with one live worker or insufficient CPU for a separate foreground
reservation reports a typed preparation-capacity refusal immediately. This proves
non-submission and permits neither a busy retry nor direct rebinding. Foreground
continues to use the serial worker. Temporary occupied capacity retains bounded
busy retries. Memory-footprint refusal also remains known unsubmitted without
escaping admission through a direct worker.

Worker count still derives from the smaller of the historical 21 GiB pool budget
and current memory remaining after host/cgroup headroom. Explicit `--workers`
selects count; `--rss-ceiling-mb` remains a settled-request rotation threshold,
not live admission. Startup rejects a pool whose measured warm-worker footprint
cannot fit its actual budget, before spawning workers or publishing a socket.
Live admission counts idle retained RSS and the remaining growth to the measured
warm footprint, separately from RSS rotation. Logs retain sizing and each admitted
slot, workload, jobs and capabilities. This extends the daemon's existing owner;
there is no second supervisor, global budget registry or jobserver.

Mutable GHC interface/object products are private to a daemon epoch and worker
slot below the requested logical build-products root. Successive requests and
worker rotations reuse that slot's directory only after its former child is
reaped. Independent daemons and direct invocations receive fresh namespaces;
disk warmth across daemon reboots/direct invocations is deliberately sacrificed
for exclusive output ownership. This transport placement preserves the logical
request used for artifact recipes and diagnostic correlation. The immutable
artifact cache remains shared. The process owner removes its private scratch
directories after its final child is reaped, preserving the logical root and
other owners. Ungraceful frontend/daemon death can leave orphan directories;
this boundary does not sweep another process's outputs.

Idle worker slots wait for accepted requests. The daemon starts resident workers
with `+RTS -I0 -RTS`: idle major collections cannot hold the retained heap when
another request arrives. Ordinary allocation-driven collections remain enabled,
as do live memory admission, settled RSS rotation and process deadlines. Finite
direct workers retain the RTS defaults. RTS startup configuration belongs to the
trusted process owner and environment; compiler request payloads do not contain
RTS arguments. Disabling idle collections also defers idle finalizers and GHC's
idle deadlock detection; the daemon's bounded operation and process owner remain
the liveness authority for blocked resident requests.
Source compilation can run compile-time IO, so daemon startup and idleness never
replay a caller's request.

The spawn counter counts logical extractor invocations, including requests
served by a resident worker. It is an observability API, not a process-fork
counter.

The native owned-daemon runner publishes the PID, producer and epoch from its
preflighted daemon to both the child measurement environment and lifecycle
report. It refuses inherited compiler ownership coordinates. Test this handoff
through the actual frontend as well as its projections; shell-owner evidence
does not qualify the native owner. Artifact retention, diagnostic completeness,
workload success and acknowledged cleanup remain independent observations.

Every process edge in the extractor chain uses the crate's parent-death
contract. Killing a caller, frontend, or daemon must reap its frontend or GHC
worker descendants; process-tree ownership is not delegated to test scripts.

## Wire boundary

Compiler stdout and stderr share a 16 MiB response payload budget. Frame
lengths are checked before allocation in direct and daemon transports; oversized
accepted responses remain indeterminate and cannot be replayed. The worker
checks capture file sizes before reading them and returns a bounded infrastructure
failure for oversized or changing captures, without truncating successful data.
Compiler artifacts remain file-backed. This bounds response materialization;
temporary capture files can still grow while the request handler runs, and fixed
metadata subprocess probes do not use the framed response budget.

In addition to `blake3`
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
