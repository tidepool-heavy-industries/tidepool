# Plans

**Active: [`harness-r0/`](harness-r0/README.md)** — typed yield
(`returnControl @T`), session tree over the E2 stow engine, durable event
log, Datastar observatory skeleton. Exo-driven: `harness-r0/README.md` is
the orchestration source (segments, models, merge order);
`harness-r0/PRD.md` is the requirements source; each segment dir holds
the spec its subagent is pointed at.

The prior plan (`repo-review-2026-07-06`, a full-repo bug hunt: ~60
findings across every crate, fixed and merged to `ghci-session`) is
closed out; see git history (`git log --all -- 'plans/repo-review-2026-07-06/*'`)
for the full writeup.
