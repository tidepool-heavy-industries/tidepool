# R-resources audit checkpoint (2026-10-21)

Role R resources, checkout `tidepool-deepseek-g2-resources-20261021`,
branch `g2-deepseek/resources-20261021`, base `0cfb964739`.

Read-only audit and proposal; nothing was deleted, retired, unmounted or
restarted. Full report with exact measurements and the admission matrix is at
`/srv/swarm/evidence/inanna/g2-closeout-deepseek-20261021/resources/resource-audit-20261021.md`.

Key results:

- Retirement transition verified: `tidepool-llm-family-effects-20261018` is
  `retired`/`deleted`/`mounted=false`; its output path is gone; source retained.
- `/srv/build`: 129 GiB free (86% used). Host RAM 125.7 GiB, 32 CPUs,
  MemAvailable ~97 GiB, swap 4.2/16 GiB used.
- build.slice: current 35.0 GiB, high 80 GiB, max 96 GiB, peak 80 GiB.
  user-1000.slice: current 12.1 GiB, high 100 GiB, max 104 GiB.
- 12 buck2d daemons (~6.3 GiB RSS): 6 idle cq-integration (1d22h), 6 active
  campaign. 3 held leases: n, q1, w.
- Registrations: 195 admitted, 182 retired, 19 legacy no-lifecycle.
- Recommendation: <= 6 focused lanes (<= 4 compile + <= 2 test/hosted),
  build.slice aggregate <= 64 GiB; one joined qualification lane with
  frontend <= 64 GiB, explicit delegated-service limits, >= 20 GiB host
  reserve, 40 GiB disk reserve.
- Standby reclaim (propose only): cq-integration (13 GiB + idle daemons),
  llm-family siblings (~24.5 GiB), older completion/demand sets (~100+ GiB).
- Warm reuse: campaign checkout is the joined warm output; preserve interactive
  `tidepool` (108 GiB); V prepares commands without a duplicate cold build.
