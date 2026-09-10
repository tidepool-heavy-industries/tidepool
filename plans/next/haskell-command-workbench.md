# Haskell command workbench

Implementation wave: [command jobs checklist](command-jobs-implementation.md).
That checklist owns completion evidence; this document records the intended
surface and follow-on boundary. Linux is the current target. A future macOS
installation may run the complete workspace in a Linux VM; no native macOS or
Windows resource backend belongs in this wave.

## Programmable commands

Commands are inspectable, transferable Haskell values. Rust owns execution,
resource admission, PTY/stdin, retained output and descendant custody through the
existing native TUI process owner. Receiving a command description does not grant
execution authority. A receiving owner executes it under its own grants.

```haskell
let focused = withMemory (GiB 4) [bash|just test-lib tidepool-node 'test(command_oom)'|]
job <- Cmd.start focused
```

`Cmd` is the qualified `Tidepool.Command` import. `Cmd.describe` exposes the
command description; `Cmd.argv [program,arg1,arg2]` avoids a shell. Bash quoting
is literal, without implicit Haskell interpolation. `Cmd.withArguments` supplies
actual positional arguments, including spaces and shell metacharacters.
`Cmd.inDirectory` and `Cmd.withEnvironment` select per-command overrides.

`Cmd.start` returns an owned job immediately. `Cmd.run` waits up to one second,
including admission, returning `Finished` with result/bounded output or `Pending`
with the continuing job. `Cmd.await` explicitly waits for completion. Jobs survive
tool return and observer disconnection; queues have no implicit lifetime timeout.
There is no automatic execution retry.

The default hard limit is 256 MiB and the same value is the admission weight.
An explicit memory override is the only model-facing resource setting. One
per-user owner shares 8 GiB general capacity plus 512 MiB protected capacity for
commands at most 256 MiB across runs. General admission is FIFO; protected small
commands can progress while a large command waits. Aggregate command swap is
1 GiB. Descendants retain their grant after the root command exits.

Nix daemon work is outside the requesting shell's subtree. Its separate budget
is 8 GiB memory and 1 GiB swap. Configure one build/one core and a two-CPU
aggregate limit. On NixOS, configure `nix.settings` and the daemon's systemd
service declaratively; runtime systemd properties can activate caps without
restarting active builds. These daemon limits do not attribute its memory to
individual clients.

## Interaction and routing

Use `Cmd.withStdin` for pipe input, or `Cmd.withTerminal` for a PTY initialized from the owning TUI dimensions.
Retain the job for `Cmd.sendInput`, `Cmd.closeInput`, `Cmd.resize` and `Cmd.cancel`.
Cancellation accepts intent; terminal status and cleanup evidence remain distinct.
Output reads are bounded tails with explicit truncation. Write large durable logs
to chosen workspace files. No output-stream subscription DSL is required.

`Cmd.completion job :: R.EventSource Cmd.CommandResult` feeds the existing
record-shaped Haskell actors. A handler receives one retained terminal value,
including when attached after completion. Use actor composition for known
continuations and message a model when judgment is needed. Command jobs themselves
are lightweight Rust resource actors, with no GHC or model session per process.
Native fallback tools keep their existing process-session owner and deferred
admission handle rather than adding a competing process registry.

## Adoption and acceptance

Shipped guidance directs builds/tests and potentially expensive execution through
Haskell. Native shell fallback remains fixed at 256 MiB; `apply_patch` remains
available. Disable the competing JavaScript wrapper while preserving direct tools.
The shared guide supplies a minimal example; the command skill carries occasional
stdin/PTY/routing details. Freeze executable and prompt changes at a swarm boundary.

Acceptance uses a scripted local provider with the actual native TUI and Shoal
binaries, plus the real Haskell, namespace and cgroup boundaries. Cover delayed
admission, small-command progress, OOM with subsequent steering, cancellation,
stdin/PTY, bounded output, argument fidelity and retained completion. Focused
resource tests additionally challenge cross-host ownership, disconnected observers
and descendants retaining capacity. No paid inference or full workspace suites.

## Later work

Keep transactional editing in [typed file tools](../typed-file-tools.md), not
this wave. A future `edit path $ do ...` could apply several precise edits or none,
with explicit stale-source and concurrent-writer semantics. Preserve native
`apply_patch` while that design matures.

Improve from actual use. Retire native shell fallback only when effectively
unused and recovery/interaction needs are covered. More detailed scheduling,
output subscriptions and dashboards should follow demonstrated needs rather
than expand the initial interface.
