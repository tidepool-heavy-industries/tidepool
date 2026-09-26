# Wave 12 completed-root latency follow-up

Run `7d0bd907-1640-49b1-a952-f98005ca6e2d`. Observed 55 completed root cell timing records, of which 23 exceeded 10 seconds. This counts completed root cells only, not child calls, native turns or provider model rounds. Evidence read through 2026-09-26 04:27:36 UTC; the last slow cell ended at 03:17:22 UTC.

Compiler values below are summed phase durations from the enclosing execution spans. They are components of cell wall time; checkout holds overlap compiler work. They must not be added to cell totals. The earlier report retains worker-phase attribution and its narrower cutoff.

| End UTC | Execution | Cell seconds | Preflight seconds | Compiler response seconds |
|---|---|---:|---:|---:|
| 02:09:08 | `exec-9a457170ff273f6dfc264e7c431ebcdb` | 24.1 | 0.0 | 6.8 |
| 02:10:09 | `exec-2990544e1cc5cd2c0982e673d70d3451` | 12.0 | 0.0 | 2.8 |
| 02:13:29 | `exec-3caf4b9038168aca06cee83d606cd122` | 11.6 | 0.4 | 4.1 |
| 02:19:45 | `exec-a1ac505e1cf30c0dac9b921757982a76` | 236.1 | 98.0 | 101.2 |
| 02:24:59 | `exec-d0ceb51583aa625fee3c4c565ba74201` | 292.0 | 99.8 | 169.7 |
| 02:28:41 | `exec-7d6e018d4a9978943fa9caec207c2bfb` | 191.9 | 0.0 | 150.5 |
| 02:29:37 | `exec-4867a6ba403de9fe192c2ec7b84a102d` | 42.8 | 0.0 | 26.9 |
| 02:31:31 | `exec-634bd91a78d184456712254b8f0699bb` | 102.3 | 19.1 | 69.8 |
| 02:32:06 | `exec-e82b53032a0d2b35231ca036a4d8e8ab` | 17.2 | 0.0 | 12.6 |
| 02:33:13 | `exec-4402a679f1cd72e0f9fa0e77aa737c6a` | 16.3 | 3.1 | 4.1 |
| 02:33:56 | `exec-2290c8388bcbb954deb56e4629fcb461` | 26.3 | 1.6 | 9.9 |
| 02:35:49 | `exec-6363b414ac7f8c38cf4c642addb7301c` | 12.0 | 3.3 | 6.5 |
| 02:37:05 | `exec-57cfe7ac71c7bef8b6de8e91451f0993` | 13.6 | 1.0 | 8.9 |
| 02:37:35 | `exec-e44dd76c34138adff05b23e10f5f8cc6` | 10.6 | 0.0 | 6.0 |
| 02:38:14 | `exec-3a8de014193324609cb403f918addd38` | 10.2 | 0.0 | 6.4 |
| 02:44:44 | `exec-d40f6f8c16183d4dd462c1ae1c277b62` | 11.2 | 0.0 | 8.7 |
| 02:45:53 | `exec-3234532825a3efd43b6a2f60005f239c` | 12.1 | 3.1 | 7.1 |
| 02:49:17 | `exec-58b371d869c782045c17eb505d95f3b1` | 73.7 | 2.1 | 69.2 |
| 03:01:51 | `exec-2ee2684c613fa9ce428e66f96265c504` | 13.7 | 0.0 | 9.3 |
| 03:02:30 | `exec-95ea0d0c7105bbcbd6e8a67285e95e32` | 18.3 | 0.0 | 9.9 |
| 03:03:20 | `exec-3837048df3bc2a67fc686858a6c0ec90` | 13.0 | 0.0 | 9.7 |
| 03:04:32 | `exec-fa48427fe798cb55d3240f51912c2b6e` | 12.5 | 0.0 | 4.2 |
| 03:17:22 | `exec-b421d4c5d1c8f6a8cf93e44823312bd8` | 157.5 | 0.0 | 13.1 |

## Interpretation and action

- Early long calls include confirmed compiler preflight starvation and prepared-projection traversal. Responsive control/backpressure fixes are integrated; projection reuse is undergoing its required corpus gate. No production speedup has yet been measured.
- The 02:49 reply observation still spent about 69 seconds in compiler response. It warrants the same phase decomposition; it is not evidence that the model was reasoning for that duration.
- The final cleanup cell spent about 13 seconds in compiler responses and approximately 145 seconds executing sequential cleanup effects. Independent host resource release waits can overlap within a group after descendant-first terminal publication; that fix is integrated. Separate Haskell cleanup calls remain sequential.
- All seven worker applications later released their resources. The root is idle with no pending requests, but the live browser demo still belongs to its command namespace/cgroup. Retain this completed run host to preserve the demo until a deliberate independent launch is available.
- Root retrospective is committed in harness `cce811a`. Exact review and expected-red consumer checks caught real defects; helper publication produced no demonstrated notebook reuse.

## Evidence limits

Native task_started/task_complete events count turns, not provider model rounds. Earlier Luna counts omitted Haskell/custom calls and must not be used as total interaction denominators. Notification presentation latency is measured separately; no aggregate token saving has been established. Raw source cells and native transcripts remain private local evidence.
