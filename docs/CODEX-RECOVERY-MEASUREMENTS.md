# Codex recovery integration measurements

These measurements were taken on 2026-09-22 from
`work/codex-recovery-consolidation` on Linux with Rust 1.93.0 and GHC 9.12.2.
They are single-machine observations from debug test binaries unless stated
otherwise. They establish bounded costs and reproducible probes; there is no
matched pre-change baseline, so they do not establish speedups.

| Boundary | Workload | Observation |
|---|---|---|
| Private command protocol | 100,000 JSON encode/decode round trips of a generation-bound terminal response | 198-byte message; 5,598 ns per round trip |
| Resource status polling | 2,000 observations with no history, then with 100,000 retained terminal entries | 3,728 ns and 3,093 ns per observation respectively; 68,136 KiB RSS added by the retained records |
| Retained output | Push 256 MiB in 64 KiB chunks through one stream ring | 25,742.6 MiB/s; 4 MiB retained and 252 MiB reported dropped |
| Run storage status | 100 bounded walks over 8,000 eight-byte files | 8,001 entries and 64,000 file bytes observed; 6,066 microseconds per sample |
| Matched release build | `nix build 'path:.#shoal-unwrapped' --no-link` from an uncached changed source | Approximately 15 minutes; Nix built the filtered Cargo vendor input and `shoal-unwrapped` |

The resource polling result is the structural claim that matters: each poll
walks active allocations and reads constant-time counters for historical,
retained, and failed-cleanup totals. Retaining another terminal result consumes
memory, but does not add it to the polling walk. Output retention is also fixed
per stream by its ring capacity, with absolute offsets recording loss.

The run storage observation intentionally stops after 8,192 entries and reports
truncation. The measurement used 8,000 files and therefore remained below that
bound. The resource status also bounds its process census at 4,096 entries.

Focused verification on this machine took 0.017 seconds for 13 command-resource
and output tests, 0.058 seconds for two command-protocol failure tests, and
0.006 seconds for the bounded storage test, excluding compilation and Nix shell
startup. A full host startup was not timed because the acceptance boundary
forbids disruptive runs against active sessions and credentialed model runs.
The model-free `actor_spec_cost_measurement` was attempted, but failed before
activation on the repository's existing extractor nominal-identity error:
`JSON True constructor does not match its admitted nominal identity`. Its
13.948-second failing run is therefore not reported as a startup observation.
The ignored measurement tests provide explicit commands for repeating the
model-free portions at later integration boundaries.
