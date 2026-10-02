# Command output retention

`CommandJobs.entries` in `exomonad/actor/src/command_jobs.rs` retains completed
jobs until the registry is dropped. Dropping a `CommandConnection` removes an
observer, not the registry entry. The facade backend in
`bridge/facade/src/actor_host/commands.rs` retains its `HostCommand`, including
stdout and stderr buffers capped at 4 MiB each by `exomonad/node/src/host_command.rs`.
The source-derived output bound is therefore about 8 MiB per retained host job,
plus capacity slack and metadata; actual retained bytes have not been measured.

Measure completed-job counts and output retention over a representative long run
before choosing a budget or release policy. Late status and output reads depend
on these retained handles, so eviction requires an explicit release or expiry
contract through the existing command owner. Preserve active observers, ownership
transfer, outstanding cleanup, and output-page cursor semantics. An expired read
must return a typed unavailable result and must never rerun the command.
