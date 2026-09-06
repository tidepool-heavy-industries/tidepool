# Prepare Shoal for the shoal-repl run

Status: preparation implemented; focused deterministic checks passed.
The [product plan](recursive-context-collaboration.md) records the accepted
direction and full scenario. This document is the bounded implementation brief.
Preserve the existing draft changes. Complete preparation before starting the
user-steered `shoal-repl` application build; no live provider campaign is needed
to finish the deterministic checks below.

The preparation scaffold is committed as `80dd0ea7`. Verification and final
review continue on that scaffold; final evidence is recorded below.

## Settled decisions

- Shared understanding stays with coordinators; implementation and repair
  histories stay with retained specialists. Scaffold / fork / fold / repeat
  applies recursively, without a prescribed tree shape or minimum task size.
- Coding actors can recurse. Effect narrowing and remaining descendant budgets
  determine limits; “coding” does not mean “leaf.” Scaffolding is a prompt
  emphasis with the same coding capabilities, not the only recursive role.
- A reviewer fork inherits the parent's current understanding and drives typed
  repairs directly with a retained implementer. Each request settles before
  the next exchange. The parent receives a verdict or consequential decision,
  not a relay of the repair conversation.
- Improving the environment is part of ordinary work. Prefer existing tools
  and small resident definitions. `[bash| ... |]` and committed, auto-included
  `.shoal/Helpers.hs` remain future ideas, not preparation requirements.
- Root behavior is staff-developer-like: autonomous execution, concise outcomes,
  considered architectural options at the agreed autonomy boundary. No dashboard
  simulation in the model conversation and no broad benchmark campaign.

## Draft inventory and ownership

| Area | Current draft | Remaining acceptance |
|---|---|---|
| Shared and role prompts | Reworked `prompts/shoal/tree-practice.md`, root and child prompts; shared decision guidance plus focused role deltas | Review composed prompts against actual roles, verify catalog/composition, retain concise wording |
| Mounted help | Concrete four-branch/recursive example in `docs/tree.md`; direct peer repair in `docs/refinement.md`; independent watches, inherited effort, declaration-group syntax | Execute changed examples through the real workbench; keep fixtures synchronized with the published text |
| Coding effect row | `haskell/actors/Tidepool/Actors/Role.hs` expands `CodingEffects`; `ScaffoldEffects` shares it | Compile facade consumers and retain rejection of explicitly narrowed operations |
| Runtime role policy | `tidepool-actor/src/role.rs` shares recursive coding capabilities; `resident_actor.rs` attenuates descendant budgets for children exposing `Forks` | Verify launch, budgets, narrowing, and fresh/context-fork paths at this owner |
| Worktree authority | `tidepool/src/actor_host.rs` gives coding actors the existing allocation/integration grant | Verify owned-worktree allocation/integration while sibling/source write boundaries and role attenuation remain intact |
| Prompt identity | Host prompt catalog bumped to v5; coding profile to `coding-v2` | Verify effective prompt fingerprints and role projections; no serialized role names are removed |
| Runtime tests | Existing recursive reply/watch test now uses a coding coordinator and exercises the published repair request/watch from its reviewer child | Compile and run; this is protocol/ownership evidence, not proof of model review quality or a code-changing repair |
| Public surface tests | `coding_cannot_unfold.hs` replaced by `coding_can_unfold.hs` and positive compile expectation | Compile/run the owning integration target; retain negative narrow-research test |
| Launcher | `just shoal-repl` forwards to `scripts/shoal-init.sh` with `~/dev/shoal-repl` | Missing-project refusal and argument routing; fresh project setup and actual launch remain separate |

Do not introduce another role registry, scheduler, process launcher, or worktree
resolver. Read the owning contributor guidance and extend the mechanisms above.
The current draft retains the scaffolding role's wire identity; removing it is
not required to make coding actors recursive.

## Implementation review priorities

1. Finish the coding-role change as one coherent boundary. Check Haskell row,
   Rust role ceiling, launch-time budget attenuation, worktree grant, generated
   facade routing, status, and prompt agreement. `ActorDescriptor::new` also
   defaults to coding: review fresh-agent and lower-level construction as well
   as `unfold`. Recursion must remain bounded; adding an effect must not bypass
   runtime authority. Budget exhaustion should be explained as exhaustion, not
   incorrectly attributed to an inherently nonrecursive coding role.
2. Run the existing recursive fixture, which now forks a coding coordinator
   and then coding descendants. The draft checks descending depth and integrates
   child commits. Ensure allocation and integration actually work under the
   coding grant. Preserve focused coverage of insufficient budget, an explicitly
   narrowed row, and inspection-only authority.
3. Check direct repair ownership. The reviewer uses an inherited implementer
   reference, owns a new repair response/watch, remains in its original review
   request, receives repair, then replies to its parent. The draft uses a small
   typed value to exercise this path; it does not implement a generic review
   protocol or claim that a changed Git candidate has been accepted. Verify
   terminal failure/cancellation behavior using existing focused checks, adding
   a missing assertion only where this topology needs it. Do not create circular
   queued requests or use progress as a message queue.
4. Review prompts as their recipient. Can an actor identify its next useful
   action, locate the necessary help, preserve its contract, and recover without
   rereading a manual? Keep motivations and decision rules in shared guidance;
   detailed examples belong in mounted help. No new blanket permission ritual,
   mandatory schema, fixed branching rule, or speculative API belongs there.
5. Review the final diff and record the exact checked revision. Update
   `SHOAL.md` with the verified recursive coding behavior and launcher usage.
   Rebuild/restart at a clean boundary before judging changed prompts live:
   a running host retains its embedded prompt and Haskell snapshot.

## Focused verification

Use the repository's matched Nix/extractor setup and one build lane. These are
the relevant entry points; run them sequentially, fixing failures before moving
on. Compile every changed target. Do not substitute a broad suite or live swarm.

```sh
just test-lib tidepool-actor 'test(role::tests) | test(prompt_catalog)'
just test-target tidepool shoal_action_surface
just test-lib tidepool 'test(prompt_catalog) | test(root_instructions_preserve_idle_and_resume_contracts) | test(actor_workspace_recipes_distinguish_orchestrators_from_coding_workers)'
just test-lib tidepool 'test(actor_host::documentation_tests::published_unfold_watch_and_request_examples_execute)'
just test-lib tidepool 'test(typed_reply_settles_response_and_wakes_registered_watch)'
```

The last test is the existing extractor-heavy recursive fixture, not a cheap
unit test. Its new peer segment executes the actual refinement documentation;
do not replace the example with a separately maintained escaped Haskell string.
The existing documentation tests also cover queued rejection and reattachment;
use those focused filters if the launch review exposes unresolved concerns.
Run formatting and `git diff --check`. No extractor translation or serialization
change is intended; if scope reaches that boundary, run `just fixtures-check`.

Verification recorded during preparation:

- `cargo fmt --all` and `git diff --check` passed for the draft at that point.
- `just --dry-run shoal-repl -- --session shoal-repl-dev --no-attach` passed.
- The first `just test-lib tidepool-actor ...` command above was deliberately
  interrupted during dependency compilation when the user redirected this turn
  to plan/prompt preparation and handoff. Exit 130; no selected tests ran and
  the changed targets are not claimed compiled. The script stopped its test
  process and tore down its extractor daemon.
- No provider run, TUI implementation, fresh project creation, or launch has
  occurred. Runtime, public-surface, and changed-help execution remain unverified.

## Launcher and project handoff

`just shoal-repl` launches the Shoal development harness against the new project;
it is not a command to run an already-built TUI binary. It uses the existing
matched build/launch script and expects a Git repository at `~/dev/shoal-repl`.
After completing preparation, use the existing `shoal new` command to initialize
that fresh project if absent. Do not repurpose `shoal-console` or implement a
second initializer. The recipe explicitly selects `gpt-6-astra` at medium effort for the initial
application campaign. Use `just shoal-init` for other launch settings.

The application brief is already settled enough to start later: `shoal-repl` is
a fresh standalone Rust thin client presenting the operator's persistent
GHCi-style workbench. It offers an expressive multiline composer, basic history
scrolling, beautiful syntax-highlighted text, and polished tmux interaction.
Use existing editor/components rather than DIY foundations. The host owns
Haskell semantics, live state, actor operations, and authority. The user will
steer the application design while it is built; no editor crate, client protocol,
or detailed operator-session architecture is selected in this preparation.

The preparation is complete when prompts/help and runtime roles agree, the
focused checks pass, launcher behavior is clear, and a reviewed revision is
ready to start the application campaign. Report remaining limits explicitly;
do not call the run ready solely because the plan and prompts have been written.

## Implementation ledger

- [x] Review the recursive role boundary and composed prompting drafts.
- [x] Explain depth exhaustion accurately at fork admission.
- [x] Execute role, facade, prompt, mounted-help, and recursive repair checks.
- [x] Verify launcher preflight and build the matched Shoal binary.
- [x] Record evidence, review final diff, and commit preparation.

### Verification evidence after scaffold `80dd0ea7`

- Role/prompt unit selection: 5 passed.
- `shoal_action_surface`: 3 passed, including recursive coding facade and
  negative public-surface checks, using this checkout's Haskell include paths.
- Host prompt composition and workspace selection: 4 passed (including
  `root_and_worker_share_git_metadata_but_not_working_tree_authority`).
- Published workbench/unfold/watch/request examples and reattachment cancellation:
  2 passed through the resident compiler/runtime.
- Launcher dry run routes session arguments, fixed workspace, `gpt-6-astra`,
  and medium effort. Missing-project preflight correctly exits 1 before launch.
- Reviewed every shared/role/tool prompt and mounted help document. Fixed the
  integration role's hard-coded checkout path and inconsistent browse-first
  tool guidance. Catalog v5 and coding-v2 identify the changed prompt policy.

- Recursive coding/reply/watch fixture: 1 passed in 105 seconds. It exercises
  coding grandchildren, commit integration, and reviewer-owned typed repair
  while the original review request remains pending. This proves protocol
  behavior, not model review quality or provider cache performance.

- `just shoal-init -- --help`: passed. Built/validated this checkout's extractor
  frontend and GHC worker, built the Shoal binary in `.#shoal`, and executed
  CLI help without starting a provider or tmux session.
- Rust formatting and `git diff --check`: passed. No extractor translation or
  serialization changes; fixture regeneration was unnecessary.
- No application was created or launched. Initialize `~/dev/shoal-repl` with
  `target/debug/shoal new ~/dev/shoal-repl`, then use `just shoal-repl` to start
  the user-steered Astra medium build. The application brief above remains the
  contract; editor and protocol architecture are decisions for that build.
