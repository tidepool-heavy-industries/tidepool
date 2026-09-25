# Wave-5 launch record

Filled at launch from verified revisions, never from memory. Every field is
either a value with its source command, or `pending`.

| Field | Value | Source |
|---|---|---|
| Run ID | c22fa217-bc6c-4d53-8e84-99e098a7eb52 | `exomonad host` output line at launch |
| Deployed binary revision | 1884c03c8 (tidepool main; redeployed 2026-09-24 19:55 local, extract stamp 4e938061) | `git -C ~/dev/tidepool rev-parse HEAD` at `scripts/redeploy.sh` |
| Workspace pin | a5bbb3b (scaffold DEFAULT_WORKSPACE_REV and this checkout's submodule) | `DEFAULT_WORKSPACE_REV` in `bridge/facade/src/exomonad/scaffold.rs`; `.exomonad/workspace` submodule here |
| Harness HEAD | 3b5a526 at launch (a4f3cb9 is the root's first commit) | `git rev-parse HEAD` in this checkout |
| Core prompt catalog version | 38 | `CATALOG_VERSION` in `bridge/facade/src/actor_host/prompt_catalog.rs` |
| Project prompt revisions | c5c98ea | `git log -1 --format=%h -- .exomonad/prompts` here |
| Log path | .exomonad/logs/c22fa217-bc6c-4d53-8e84-99e098a7eb52.log | `.exomonad/logs/<run>.log` |
| Observer owner | Fable (session cef248c1), 30-minute wakes; Astra reviews | named at launch |
| Wake job | see below | cron id and cadence |
| Launch time (UTC) | 2026-09-25T02:58Z | `date -u` |

Mid-wave prompt edits append a row below with the revision, the time, and
the actors known to have received it.

| Time (UTC) | Revision | Files | Actors exposed |
|---|---|---|---|

Execution model: co-resident (fresh machines built in but disabled: no child
bootstrap program is installed). Root model gpt-6-sol medium. Brief: "Read
NEXT.md in the repository root and follow it..."
