# What is installed

Every module below is compiled into every session in this workspace, verified by
`exomonad check --workspace .` from the run directory. `lookup <name>` browses any
of them and is authoritative over this file.

The shipped template is the canonical source for these modules. This repository's
`.exomonad/config.toml` points at `exomonad/examples/workspace/.exomonad`, so the
modules and recipe checks are maintained once and arrive in worktrees by checkout.

## Orchestration

| Module | Enabled | A working invocation |
|---|---|---|
| `Project.Types` | yes | `Task { taskGroup = group, planPath = "…", obligation = "…", ownedPaths = […], acceptance = "…" }` — the shared vocabulary; also `Candidate`, `Outcome`, `Question`, `WorkProgress`, `RepairOwner`. |
| `Project.Actors` | yes | role and effect aliases used by the branch constructors below. |
| `Project.Work` | yes | `implement :: Task -> Eff effects (Response (Outcome Candidate), Progress WorkProgress)`; also `solTask`, `solTaskFrom`, `reviewCandidate`, `reviewAgain`, `repair`, `requestIncorporation`, `consultDesign`, `projectPrompt`. |
| `Project.Routing` | yes | collection patterns over several children's replies and questions. |
| `Project.Observe` | yes | `observeWork` for a read-only snapshot of work in flight; `workSummary` renders it without consuming it. |

## Judgment and integration

| Module | Enabled | A working invocation |
|---|---|---|
| `Project.Reflex` | yes | `classify :: Text -> Maybe Reflex` — classify compiler, lint or test output from a precedence-ordered table, no model turn. `reflexFor :: Int -> Text -> Maybe Reflex` takes the exit code too. |
| `Project.Evidence` | yes | `coverageCheck :: Text -> Text -> CheckResult`; `CheckSource` keeps `ChildReported` separate from `RanHere`. |
| `Project.Contract` | yes | `Contract`, `ImplReport`, `ImplNote`, `defaultReviewPolicy`, `renderBrief :: ReviewBrief -> Text`. |
| `Project.Investigate` | yes | `investigate :: (Member Jev effs, Member Commands effs) => InvestigationPolicy -> Text -> Text -> [Text] -> [Text] -> [Text] -> Text -> Int -> Text -> Eff effs Investigation` — policy, directory, oid, owned paths, requirements, intent, command, exit code, output. |
| `Project.Merge` | yes | `mergeInto :: WorktreeId -> Maybe BranchName -> [Text] -> ActorSpec Merge MergeEffects`, started as `R.start (mergeInto (worktreeId tree) (Just "exomonad/integration") ["just","test-lib","exomonad-actor","test(request::updates)"])`. The check is an argument: name the narrowest command that would catch a regression in the change at hand, never `just verify`. Checks a merged head before publishing and rolls a red one back. |
| `Project.Review` | yes | `reviewOf :: Contract -> (Response ImplReport, Progress ImplNote) -> MergeTarget -> ActorSpec Review ReviewEffects`, started as `R.start (reviewOf contract worker (MergeTarget merge))`. |

The same source root supplies `Project.Shell` and `Project.Lookup` for typed tool
selection, plus `Project.Search`, `Project.History`, `Project.Service` and
`Project.Repository` for the repository-reading examples. `AgentSpec.agentSpec`
installs `Project.Watchdog.watchBy` as its after-tool handler: children labelled
`escalate-child` get an out-of-scope escalation, children labelled `nudge-child`
get repeating-failure advice, and other actors abstain.

## Not installed, deliberately

`Project.Plan` generated assignments for one graph-UI feature. It was retired
rather than carried forward as though it were a general tool. Its campaign
tree is in Git history.

The recipe list is shared with the shipped template. Its current-head status is
pending the Nix-backed Exomonad checks after the toolchain fork is pushed.

## Prompts and skills

The root runs on the **shipped** `base.md` + `api-guide.md` and `root.md`. This
workspace sets no `[prompts] core` or `root`, and none of the reserved child keys
(`research`, `coding`, `scaffolding`, `integration`), so children get the shipped
role prompts too.

`[prompts.files]` carries only what this workspace's own Haskell reads through
`projectPrompt`: `task`, `review`, `repair`, `incorporate`, `specialist`, `lead`,
`rsi`.

All ten skills resolve through `.agents/skills/`, which points at the shipped
originals in this repository rather than at a second copy.

## Worked cells

`.exomonad/plans/examples/` holds six cells with the fixtures they read —
reference to copy from, not modules to import. Its README says which run here
unchanged and which were repointed at this repository and not re-run.

## Verification boundary

The prior configuration was checked before this source consolidation. The
consolidated workspace and its recipes must be checked against the current head
after the Nix toolchain becomes available; the moved examples have not been
executed since being adapted.
