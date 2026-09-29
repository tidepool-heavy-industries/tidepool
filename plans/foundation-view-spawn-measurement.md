# Retained-view command launch: bounded measurement

The compared production paths are `MountNamespace::host_command` (a host
`Command` with namespace setup in `pre_exec`) and
`view_command::output_in_view` (a `posix_spawn` of the small
`exomonad-view-helper`, which enters the same retained view before exec).
`compare_host_fork_with_view_helper_spawn` runs both against one retained
overlay view and executes `/bin/sh -c :`. It alternates order across 100
pairs after 20 warm pairs. Output capture and process wait are included.

On 2026-09-28, this machine had an Intel i5-12600K, 16 logical CPUs, 31 GiB
RAM, and Linux 6.12.63. The test was built with `cargo test -p exomonad-node
--test overlay_rotation --no-run` and the helper with `cargo build -p tidepool
--bin exomonad-view-helper`, using the shared Cargo target. The two runs used
the repository Nix shell and these arguments to the ignored test:

```
VIEW_SPAWN_BENCH_PAIRS=100 cargo test -p exomonad-node --test overlay_rotation compare_host_fork_with_view_helper_spawn -- --exact --ignored --nocapture
VIEW_SPAWN_BENCH_RSS_MIB=1024 VIEW_SPAWN_BENCH_PAIRS=100 cargo test -p exomonad-node --test overlay_rotation compare_host_fork_with_view_helper_spawn -- --exact --ignored --nocapture
```

| Touched test-host RSS | Legacy p50 / p95 | Helper p50 / p95 |
| --- | ---: | ---: |
| 0 MiB | 9.46 / 11.08 ms | 11.76 / 14.10 ms |
| 1024 MiB | 23.85 / 37.69 ms | 5.01 / 10.63 ms |

An additional three-pair run at 1024 MiB used `strace -ff -ttt -T -e
trace=clone,clone3,fork,vfork,execve,execveat,setns` on the already built
test executable. The legacy child was created with `clone(... SIGCHLD)`;
three observed clone calls took approximately 6–10 ms. The helper was
created with `clone3(CLONE_VM|CLONE_VFORK|CLONE_CLEAR_SIGHAND)`; three
observed calls took approximately 0.16–0.62 ms. The helper then executed,
entered the user and mount namespaces with `setns`, and executed the payload.
The trace also showed three output-reader threads per helper invocation.

These are one-machine measurements of a synthetic touched allocation, not
measurements of the live JIT host or of production Git commands. The two
untraced runs occurred sequentially without controlled cache or CPU state.
They support the narrower mechanism claim that the helper avoids the
large-host fork path; they do not establish a production speedup. A live-host
measurement on the planned 128 GiB machine remains open.
