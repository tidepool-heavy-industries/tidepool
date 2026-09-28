# Wave21 stall investigation — 2026-09-28

Run `a091048d-3c51-4686-b49e-c7d917807dd5`, launched about 07:43 UTC.
Investigation began 15:17 UTC. This is an incident report, not delivery evidence.

## Findings

The host log ends at 09:08:26 UTC: roughly 85 minutes of observable activity,
then over six hours without logged progress. Live tmux panes were incorrectly
reported as a working wave. Root and component panes remain inside tool calls.
The passive operator actors endpoint times out after eight seconds with no bytes.

The shared swarm cgroup is at its 16 GiB memory.high threshold, with its full
24 GiB swap allowance occupied. Its memory.max is 18 GiB. Observed full memory
pressure averages were about 98%; a two-second sample gained 5,967 high events,
14,923 pages scanned, 10,318 reclaimed, and 324 major faults. There were no new
OOM kills in that short sample. Lifetime OOM counters must not be attributed
entirely to this wave.

The host cgroup CPU accounting shows 19,724 seconds in system time versus
1,800 seconds in user time. This strongly supports pathological reclaim as the
dominant present failure, not useful Haskell computation. It does not exclude
a concurrent runtime deadlock. GDB attachment was denied by ptrace policy;
unprivileged perf recordings produced no samples. No stack-level diagnosis is
claimed.

Kernel journal entries at 08:52:53–08:52:59 UTC explicitly identify wave21
command cgroups losing Cargo/rustc/linker processes to OOM. At observation two
rustc processes for actors 3 and 21 were still alive after over six hours.
The brief's one-expensive-build-slot instruction was not an enforced global
build permit in `scripts/cargo-focused-test`.

Approximate current cgroup resident/swap attribution:

| Owner | Resident MiB | Swap MiB |
| --- | ---: | ---: |
| Wave21 compiler scope | 5,887 | 769 |
| Wave21 host service | 3,675 | 3,661 |
| Command resource service | 3,003 | 113 |
| Old compiler scope, PID 233396 | 12 | 5,357 |

Dozens of native model client scopes add several GiB resident and roughly
400–500 MiB swap apiece. Full observations are in `resource-snapshot.json`.
The old compiler is shared/preserved; it was not stopped or restarted.

## Structural admission defect

`exomonad/node/src/command_resources.rs::admit_actor` reads host-wide
`/proc/meminfo` MemAvailable. Its decision checks that value against 6 GiB
headroom plus a 1 GiB transient start reservation. It does not consult ancestor
memory.high/max headroom or pressure. During this incident the host still
reported 8.3 GiB available while the swarm was reclaiming almost continuously.
Thus the admission predicate can say there is room when the actual execution
slice has none. Reservations cover pending starts, not actors' lifetime growth.
Finite slice ceilings alone provide containment, not progress guarantees.

## Earlier serialization cost

From the retained 59,312-line host log:

| Measurement | Count | Median | p95 | Maximum |
| --- | ---: | ---: | ---: | ---: |
| Machine checkout waits | 18,531 | 946 ms | 15,412 ms | 184,133 ms |
| Completed checkout holds | 18,530 | 0 ms | 320 ms | 148,549 ms |
| Compiler transaction responses | 1,716 | 1,058 ms | 4,672 ms | 86,327 ms |

Checkout waits sum to 83,634 actor-seconds; these overlap and must not be
presented as wall time. Completed holds sum to 4,611 seconds. The long holds
increase over the run. Actor 51's 148.5-second hold follows its interactive-to-
boot transition, not an identified 148-second GHC compile. The final unmatched
checkout belongs to actor 4, execution `exec-dcacb3e52d5c83b4844eb154d1361625`.
Current logs cannot split these holds into GC, installation, evaluation, and
memory-reclaim time. Compiler and checkout numbers alone do not establish the
underlying expensive operation.

## Required direction

1. Make admission respect the effective ancestor memory budget and sustained
   pressure; retain control-plane headroom. Bound concurrent runnable work while
   preserving recursive task structure. Do not equate deeper trees with unlimited
   simultaneously resident clients.
2. Enforce expensive-build admission in its owning resource mechanism. Preserve
   command identity and report OOM/refusal to the requesting actor.
3. Investigate machine growth and long boot/install holds after restoring usable
   memory conditions, with phase timings. Do not label the whole delay compilation
   or solve it by simply increasing compiler worker count.
4. Make supervision detect stale progress plus pressure/unresponsive inspection.
   A live process and pane are insufficient evidence of a healthy run.

No actors, daemons, dirty worktrees, or run resources were retired. No limits
were changed and no successor launched. Recovery requires an explicit resource
plan; blindly increasing this slice's allowance could move the failure to the
whole machine. Preserve source and evidence before any teardown.
