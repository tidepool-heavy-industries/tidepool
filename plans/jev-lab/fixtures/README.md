# Exact inputs

Both files are the literal stdout+stderr of `bash ./check.sh` run in a checkout
of `~/dev/shoal-evals/tui-test-app` at the named commit, captured 2026-09-17.
Both exited 101. They are the only inputs the investigation reads; everything
else it fetches itself with `git` against the same commit.

| file | commit | subject | shape |
|---|---|---|---|
| `f726882-check.out` | `f726882` | Add Tags focus panel to app contract | 5 x `error[E0004]` non-exhaustive patterns, one shared `note: ... defined here` at `src/app.rs:61:10` |
| `4610b5e-check.out` | `4610b5e` | Add tags panel wiring and tag startup filter | 1 clippy `field_reassign_with_default`, promoted by `-D warnings`, inside a test, note one line above the primary site |

Owned paths used in every run recorded here: `["src/panels/"]`.

To reproduce a run, launch a session against the toy repo and replay the cells
in the parent directory in numeric order; `13-final.hs` defines `look` and
`14-route.hs` defines `routeFindings`.
f72688235d1b5483e37b14f7961a9e64eb85743e
4610b5e9b87bf2f919f18f716501eca863909d6b

`53ad43c-check.out` was made later, as a third shape for the usability test:
`shoal/invtest` adds a `limit` parameter to `store::load`, and the check fails
with four `error[E0061]: this function takes 2 arguments but 1 argument was
supplied`. The two closing tallies disagree, one saying 1 previous error and
the other 4, because the binary target sees one caller and the test target sees
the rest.
