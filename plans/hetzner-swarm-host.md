# Dedicated swarm host

Status: direction agreed with Inanna, 2026-09-28; purchase and installation pending.

## Goal and decisions

Give parallel Exomonad waves enough memory that frontier model time is not spent
waiting on reclaim, swap, and overloaded builds. Keep the existing development
server available for ordinary work and recovery.

- One Hetzner dedicated x86-64 server, targeting 128 GB RAM, a modern CPU and
  two NVMe drives. AX102 is a candidate, not an order or an availability promise.
- Monthly hosting is acceptable. Inanna owns the account, ordering and billing;
  automation receives SSH access, not permission or credentials to buy capacity.
- Declarative NixOS configuration, disko for storage, nixos-anywhere for initial
  installation. Terraform is unnecessary for this one manually ordered host.
- Keep current runs on their current host. Validate the new host with a subsequent
  bounded wave before making it the normal swarm machine.

The fixed server rental makes the base expense predictable, but is not a prepaid
hard spending cap. Confirm tax, setup, IPv4, add-ons, cancellation terms and the
final monthly total before ordering. No purchase is authorized by this document.

## Existing configuration to reuse

Inspected `/etc/nixos/configuration.nix` and `hardware-configuration.nix` on
2026-09-28. They are in a local Git repository. The current host uses ext4, an
Intel i5-12600K, about 31 GiB usable RAM, and about 40 GiB configured swap.

Reusable policy already exists for SSH, Tailscale, Nix cache trust, build budgets,
and `swarm.slice`. Extract reviewed modules rather than copying the whole system:
the current configuration also contains unrelated personal/media services.
Keep host hardware, disks, networking and resource budgets separate from shared
development and swarm policy. Review configuration for secrets before publishing.

Current limits are **not suitable defaults for the new host**:

| Owner | Current configuration | New-host decision |
| --- | --- | --- |
| Swarm user slice | high 16G, max 18G, swap max 24G | Size for measured working sets and OS/control-plane headroom |
| Nix daemon | max 8G, swap max 1G, CPU quota 200% | Give builds an explicit separate budget |
| Nix parallelism | two jobs, six cores/job | Tune within build memory and CPU budgets |
| Compiler worker | run-owned worker, currently one worker | Preserve ownership and bounded concurrency initially |

Limits are ceilings, not reservations. Account for overlapping runtime and build
use. More RAM will not fix retained actors or unbounded source copies; keep the
retirement, storage lifecycle and observation work in place.

## Implementation sequence

1. **Inventory and prepare configuration.** Retain the existing host's exact
   configuration revision and local differences. Create a versioned host flake
   with small shared modules; pin nixpkgs, disko and nixos-anywhere inputs.
   Leave this server's active configuration untouched during preparation.
2. **Resolve ordering choices with Inanna.** Confirm current stock and final
   price, location, CPU/RAM specification, disk capacities, rescue access and
   cancellation terms. Inanna orders the one agreed server.
3. **Review storage and installation.** Prefer mirrored NVMe for disk failure
   tolerance, with a separate backup. Choose the filesystem after checking our
   overlay/bubblewrap and reflink requirements against the storage owner. Do not
   assume ext4 or a mirror provides reflinks. Identify disks by stable IDs and
   review the destructive install target before executing disko.
4. **Bring up the host.** Install via rescue SSH; configure user access, Tailscale,
   firewall, Nix tooling/cache trust, systemd delegation and measured budgets.
   Keep provider credentials and Tailscale enrollment secrets out of the Nix store
   and repository. Keep administrative observation outside the swarm slice.
5. **Seed useful artifacts.** Transfer source and required Git objects deliberately;
   use existing managed source-import policy. Warm matched compiler/runtime builds
   and reuse immutable Nix artifacts. Do not recursively copy `.cache`, nested
   worktrees, or every old `target` directory. Validate path-sensitive artifacts.
6. **Validate and run one bounded wave.** Check matched toolchain execution,
   managed checkout isolation, child launch/retirement and resource release.
   Use the wave as the workload test, retaining actor counts, memory/PSS/SwapPSS,
   pressure, admission delays, notebook timing and build timing. Record exact
   revisions and pins; distinguish more hardware from software improvements.
7. **Adopt and maintain.** Make the host the normal swarm target after acceptance.
   Declare disk alerts, conservative cache/GC policy and backup jobs. Cleanup must
   distinguish disposable artifacts from dirty source, retained commits and run
   evidence. Never treat a mounted checkout's apparent deletions as authored work.

## Recovery and acceptance

- SSH/Tailscale access and rescue recovery are documented and exercised.
- Effective cgroup budgets match the new hardware; administrative access remains
  responsive under the bounded wave. Admission observes the actual limiting slice.
- The managed checkout mechanism works on the chosen filesystem; imported artifacts
  do not cause recursive copies or invalidate source isolation.
- Back up source, dirty changes, Git objects, configuration and useful run evidence
  to the existing dev server. Demonstrate restoration of a small retained worktree
  and its commit before relying on that backup. Build caches are disposable.
- A complete wave launches, makes reviewed product progress, retains its interview,
  and releases completed resources. No claimed performance improvement without
  measured timings and workload/model-mix qualifications.

## Open choices

Exact server/region and all-in price; filesystem/mirror layout; configuration repo
location; initial resource budgets and backup retention. These can be resolved
without changing Exomonad's orchestration design or waiting for the shared harness.

## References

- [Hetzner AX102 configuration](https://www.hetzner.com/dedicated-rootserver/ax102/konfigurator/)
- [Hetzner published price adjustments](https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/)
- [nixos-anywhere](https://github.com/nix-community/nixos-anywhere)
- [Storage lifecycle](storage-lifecycle.md)
- [Wave22 resource evidence](../docs/reports/wave22-audit/batch.md)
