# What is installed

Every module below is compiled into every session in this workspace, verified by
`shoal check --workspace .` from the run directory. `lookup <name>` browses any
of them and is authoritative over this file.

All eleven are canonical here — `.shoal/Project/` in this repository, tracked in
git, arriving in a worktree by checkout rather than by being copied.

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
| `Project.Merge` | yes | `mergeInto :: WorktreeId -> Maybe BranchName -> [Text] -> ActorSpec Merge MergeEffects`, started as `R.start (mergeInto (worktreeId tree) (Just "shoal/integration") ["just","test-lib","tidepool-actor","test(request::updates)"])`. The check is an argument: name the narrowest command that would catch a regression in the change at hand, never `just verify`. Checks a merged head before publishing and rolls a red one back. |
| `Project.Review` | yes | `reviewOf :: Contract -> (Response ImplReport, Progress ImplNote) -> MergeTarget -> ActorSpec Review ReviewEffects`, started as `R.start (reviewOf contract worker (MergeTarget merge))`. |

## Not installed, deliberately

`Project.Plan` generated assignments for one graph-UI feature. It was retired
rather than carried forward as though it were a general tool. Its campaign
tree is in Git history.

`checks` is unset in `config.toml`. The recipe modules that were configured
(`Project.Checks.workbench`, `Project.CollaborationChecks.collaboration`,
`Project.RoutingChecks.routing`) import a retired API generation, so
`shoal check --recipes` cannot run until they are ported. That is real available
work, not a hidden failure.

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

`.shoal/examples/` holds six cells with the fixtures they read — reference to
copy from, not modules to import. Its README says which run here unchanged and
which were repointed at this repository and not re-run.

## Verification boundary

`shoal check --workspace .` compiles all eleven against this revision with no
models or providers. That is what has been established. The cells have not been
executed since being adapted, and `--recipes` cannot run for the reason above.
