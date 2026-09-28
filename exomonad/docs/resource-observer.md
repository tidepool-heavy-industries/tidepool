# External run resource observer

Run this Python 3 script from a shell **outside** the workload's `swarm.slice`.
It reads Linux cgroup v2, `/proc`, the run's `status.json`, log metadata, and a
read-only actor graph endpoint. It does not start, stop, or query an actor cell.

```sh
python3 exomonad/scripts/resource-observer.py \
  --workspace /absolute/path/to/workspace \
  --run-id 00000000-0000-0000-0000-000000000000 \
  --run-root /absolute/path/to/state/tidepool/exomonad/runs/00000000-0000-0000-0000-000000000000 \
  --output /tmp/exomonad-resource.jsonl
```

`--once` takes one complete sample and exits. Otherwise it samples cgroup
limits, use, events, pressure, process counts, and log age every 15 seconds.
Every fourth sample it also reads PSS and SwapPSS for attributed run and other
slice processes, and reads actor counts from the graph socket within a single
two-second deadline and 256 KiB response bound. The JSONL stops at
20 MiB; `<output>.summary.json` retains the latest compact status. The output
contains no command lines, environment variables, log contents, transcripts,
or recovery credentials.

Attribution requires a matching run record. The host service cgroup and sibling
scopes with an exact run ID launcher argument identify run processes. Other
processes in the shared slice are counted and sampled separately. `run_scopes` sums
disjoint run scope cgroup memory, which includes cache; `process_memory` sums
PSS only for attributed processes. These figures have different accounting
rules and must not be subtracted from shared slice memory to estimate other
consumers. Missing/stopped services, unreadable `/proc` records, absent logs,
and stale log timestamps appear explicitly. Log silence by itself is not a
stall warning. Repeated shared slice memory pressure or actor inspection
timeouts produce warnings for investigation.
