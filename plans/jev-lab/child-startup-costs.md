# What a read-only Shoal child pays before its first useful tool call

Wave 7, stage one. Every claim carries a `file:line` citation against this
worktree. Where a number can only come from a live run, that is said plainly
rather than guessed.

## The question, restated against the code

Astra delegated six times to its provider's native subagents (5-8 s to start)
and once to a Shoal child (7m45s alive). The brief names the suspects as
"actor admission, descriptor construction, prompt composition, workbench/tool
installation, machine boot, worktree checkout". Reading the code, those are not
one path but **two**, and they cost very differently.

| | fork child (`unfold` / `child` / `Forks`) | fresh child (`startAgent` / `AgentLaunch`) |
|---|---|---|
| Haskell surface | `unfold group (child (researching @T seed (assignment lbl x)))` | `startAgent (readonlyAgent lbl)` then `request` |
| Rust decode | `resident_workbench.rs:3352` (`ForksStartWith`) | `resident_workbench.rs:3331-3350` (`AgentLaunchWith`) |
| fork group | claimed, `resident_actor.rs:1752-1760` | `fork_group: None`, `resident_workbench.rs:3344` |
| worktree | `fork_workspace: Some(..)` → `git worktree add` | `fork_workspace: None`, `resident_workbench.rs:3344` |
| starts | only after the enclosing cell returns | at the effect boundary, in the same cell |
| lifetime | as requested | `ParentOwned`, `resident_workbench.rs:3346` |
| role | as requested, attenuated | inherited-without-worktree → **research** |

Astra used the first. The second already exists, is already read-only, and is
already cheap. Most of stage one's answer is that the expensive path is not the
one a read-only question needs.

## The ordered cost list, fresh child (`startAgent (readonlyAgent …)`)

1. **Effect decode and descriptor construction** —
   `tidepool-actor/src/resident_workbench.rs:3331-3350` builds an
   `ActorStartRequest`; `tidepool-actor/src/start.rs:294-380`
   (`capture_decoded`) validates the model string (`start.rs:320-324`), mints
   the Haskell scope (`start.rs:326-338`), resolves the role
   (`start.rs:339-345`), and builds the `ActorDescriptor`
   (`start.rs:346-361`). Pure in-process. **Unavoidable**, and small.
2. **Entry custody** — `start.rs:328-330`
   (`live_payload_handle_owned_by`) plus `materialize_entry_facade`
   (`start.rs:383-400`) take exclusive custody of the child's *already
   compiled* closure out of the parent's live heap. No GHC invocation.
   **Unavoidable**, and small.
3. **Role ceiling and attenuation** — `resident_actor.rs:1722-1751`.
   In-process. **Unavoidable**: this is the authority check.
4. **Kernel admission** — `resident_actor.rs:1806-1815`; `spawn_worker`
   at `local_actor.rs:361`, `spawn_linked` for `ParentOwned` at
   `local_actor.rs:403-405`. Mailbox and registry insertion. **Unavoidable**,
   and small.
5. **Actor boot** — `local_actor.rs:749-813` (`pre_start`) runs the child's
   Haskell entry: `ResidentBoot::Entry` at `resident_actor.rs:3810-3823`,
   then the startup-step loop at `resident_actor.rs:3824-3886`.
   **Unavoidable.**
6. **Tool-record compile** — the entry's `attachAgent`
   (`haskell/actors/Tidepool/Actors/Internal/Agent.hs:645`, run from the entry at `:528`) reaches
   `ResidentActorStartupStep::Attach` (`resident_actor.rs:3873-3886`), which
   calls `install_interactive_policy` (`resident_actor.rs:3668`), which calls
   `prepare_tools` (`resident_actor.rs:3686`;
   `resident_workbench.rs:1864-1985`). `prepare_tools` composes the one-line
   fragment
   `_ <- Tidepool.Agent.Contract.installTools @(<row>) <entry>`
   (`resident_workbench.rs:1886-1890`) and runs it through `begin_fragment`
   (`resident_workbench.rs:2611-2648`) → `compile_block` → a real GHC turn
   compile. It is skipped only when the deployment set no tool record
   (`resident_workbench.rs:1868-1870`), and Shoal always sets one:
   `tidepool/src/actor_host.rs:2046` defaults it to
   `Tidepool.Command.Tools.tools`. **Ceremony for a read-only errand** — see
   below.
7. **Provider workspace preparation** —
   `tidepool/src/actor_host.rs:3486-3503` runs `WorkspaceLayout::prepare`
   (`tidepool/src/actor_host/workspace.rs:289-330+`) on the blocking pool:
   source exclusions, an imported-base overlay, and a
   `ProcessMountBoundary`. There is a reuse cache keyed on a source inventory
   and manifest (`workspace.rs:175-188`, `192-238`), so the first child in a
   run pays a full source import and later children pay two inventory walks
   plus two manifest hashes of the source tree. **Unavoidable while a child is
   a separate sandboxed process**; the cache is the existing mitigation.
8. **Resource admission** — `tidepool/src/actor_host.rs:3423-3437` waits on
   `command_resources.admit_actor()` when configured. This is a *queue*: under
   memory pressure a child waits here with the observation
   `"waiting for memory admission"`. **Unavoidable**, but a candidate
   explanation for a long wall clock that is not work.
9. **Prompt composition** — the base prompt is materialized **once per run**,
   not per child (`actor_host.rs:2275`;
   `tidepool/src/actor_host/prompt_catalog.rs:104-131`, which early-returns
   when the content-addressed file already matches). Per child it is role
   selection plus string concatenation and one blake3 hash
   (`actor_host.rs:3598-3631`; `developer_instructions_selected` at
   `actor_host.rs:5180-5224`). For a research child the role file is
   `prompts/shoal/readonly-agent.md`. **Unavoidable, and cheap** — prompt
   composition is not the cost, whatever it looks like from outside.
10. **Socket, binding, supervisor manifest** — `actor_host.rs:3527-3533`
    (`prepare_socket_inbox`), `:3685-3800`. Cheap syscalls.
    **Unavoidable.**
11. **Provider process launch** — `backend.render(&spec)` at
    `actor_host.rs:3665`, then a real fork/exec of the bubblewrap-wrapped
    provider CLI at `tidepool-node/src/process_scope.rs:241`.
    **Unavoidable, and this is the structural gap with a native subagent**: a
    native subagent is one more API request inside the provider process that
    is already running; a Shoal child is a second provider process with its
    own sandbox, socket, workspace view and cold conversation.
12. **First model turn** — the child connects back over the hosted tool
    socket and issues its first tool call. **Unavoidable.**

## What the fork path adds on top

13. **Fork-group claim** — `resident_actor.rs:1752-1760`,
    `lineage.rs:346` (`begin`) and `lineage.rs:428` (`claim`). The
    applicative batch is reserved as one group, which is why
    `prompts/shoal/docs/unfold.md:1-3` says children start only after the
    enclosing cell finishes. **Ceremony for a single read-only question**: it
    buys atomic admission of a *batch*, and a single errand is not a batch.
14. **Source checkpoint** — before a live-source fork Shoal commits eligible
    edits on the source checkout's current branch
    (`prompts/shoal/docs/tree.md:11-13`). A git commit the author did not ask
    for. **Ceremony for a read-only errand.**
15. **Worktree checkout** — `resident_actor.rs:1766-1793` →
    `tidepool/src/actor_host.rs:324-386` (`admit`) →
    `tidepool-handlers/src/handlers/worktree.rs:291` →
    `tidepool-worktree/src/create.rs:837-845`, which shells
    `git worktree add -q -b <branch> <cwd> <seed>`. A second real subprocess
    and a full checkout. **Ceremony for a read-only errand**, and it is also
    what flips the role: an actor *with* a worktree resolves to coding
    (`start.rs:486-492`).
16. **Two more model turns of latency** — the child cannot start until the
    cell returns (`unfold.md:1-3`), and the parent must then end its turn and
    be woken by a watch (`unfold.md:44-46`). For one read-only question that
    is at minimum admit-turn → wake-turn before the child's first token.
    **Ceremony.**
17. **Explicit retirement** — `planCleanup` / `executeCleanup`
    (`haskell/actors/Tidepool/Actors/Unfold.hs:417-447`), documented at
    `prompts/shoal/docs/cleanup.md`. **Ceremony** for a fresh child, which is
    `ParentOwned` (`resident_workbench.rs:3346`) and therefore `spawn_linked`
    (`local_actor.rs:403-405`): it already goes with its owner.

## Authority: the read-only child is already read-only

A fresh child launched without a worktree resolves to
`EffectiveRole::research()` — `start.rs:486-492` maps
`ActorInheritedRole` to `research()` when `has_worktree` is false, which is the
same rule `Tidepool.Actor.Record`'s own doc states
(`haskell/lib/Tidepool/Actor/Record.hs:288-292`). `research()`
(`tidepool-actor/src/role.rs:161-188`) gives:

- `NativeToolClass::InspectionOnly` and `WorkspaceAccess::InspectOnly` —
  it cannot write;
- `DescendantBudget { maximum_depth: 0, maximum_active_children: Some(0) }` —
  it cannot spawn, even though `Forks` is in its row (`role.rs:449-459`,
  `permits_child`);
- no `AgentEffectKey::AgentLaunch` and no `Source` — it cannot issue errands
  of its own and cannot publish a workspace revision.

So the authority the brief asks for is not something to add. It is what the
worktree-less path already produces, and the thing that *takes it away* is the
worktree checkout in step 15.

## What the new spans actually tell us, and what they do not

The tracing that landed writes `<workspace>/.shoal/logs/<run_id>.jsonl`
(`tidepool/src/shoal.rs:1254-1258`) through a JSON layer
(`shoal.rs:1306-1316`) filtered by `info,shoal::content=trace`
(`shoal.rs:1274-1281`). The production spans, in full, are:

| span | site | fields |
|---|---|---|
| `shoal_host` | `tidepool/src/shoal.rs:957` | `run_id` |
| `actor` | `tidepool-actor/src/local_actor.rs:820` | `actor`, `incarnation`, `message` |
| `tool_call` | `tidepool/src/host_dynamic_tools.rs:1012` | — |
| `cell` | `tidepool-actor/src/resident_actor.rs:4636` | `actor`, `execution`, `tool`, `items` |
| `unit` | `tidepool-actor/src/resident_actor.rs:4999` | `index`, `total`, `kind` |
| `cell_check` | `tidepool-runtime/src/session/turn.rs:2026` | `cell_bytes` |
| `turn_compile` | `tidepool-runtime/src/session/turn.rs:2229` | `kind`, `pinned` |
| `compile_request` | `tidepool-extract-cmd/src/endpoint.rs:245` | (client side) |

**Not one of the twelve costs above is covered by a span of its own.** There is
no span on `capture_decoded`, on `spawn_worker`, on `prepare_tools`, on
`WorkspaceLayout::prepare`, on `admit` / `git worktree add`, on prompt
composition, or on the provider process launch. Crucially,
`LocalActor::pre_start` (`local_actor.rs:749-813`) — the function that runs the
whole child boot — carries no instrumentation; the one `actor` span is on
`handle` (`local_actor.rs:820-829`), i.e. post-boot message dispatch, and its
own doc comment says so.

Two consequences, both worth fixing before the next flight:

- The tool-record compile of step 6 **is** timed — it goes through
  `run_turn_with_pin`, so a `turn_compile` span with a real duration appears in
  the jsonl. But because `pre_start` opens no span, that `turn_compile`
  arrives with no `actor` ancestor: it is an orphan you cannot attribute to the
  child that paid it. The duration is in the file; the attribution is not.
- The only artifact naming a child launch at all is a point-in-time event,
  `tracing::info!(target: "shoal::content", …, "child assignment")` at
  `tidepool-handlers/src/handlers/agent.rs:1145`, with no paired completion
  event to diff against.

**Therefore: the split of Astra's 7m45s across steps 7, 8, 11, 15 and 16 cannot
be read off the current spans, and cannot be read off the parked run at all —
that run predates the tracing.** Naming a number for any of them here would be
a guess. What can be stated from the code without a run is the *shape*: one GHC
compile (step 6), two subprocesses (steps 11 and 15), one unbounded queue wait
(step 8), and at least two model turns of protocol latency (step 16) — against
a native subagent's zero of each.

The smallest instrumentation that would close this: a span on `pre_start`
carrying the actor identity, and one each on `prepare_tools`,
`WorkspaceLayout::prepare`, and the `admit` blocking task. That is four
`#[tracing::instrument]` attributes and is not part of this parcel.

## Verdict: what is ceremony

Unavoidable for a read-only errand: steps 1-5, 7, 9-12. The irreducible cost
of a Shoal child is *a second provider process with its own workspace view*,
and nothing in this parcel changes that.

Ceremony, cut in stage two: steps 13, 14, 15, 16, 17 — fork group, source
checkpoint, worktree checkout, the two-turn admit/wake protocol, and explicit
retirement. All five come from the `unfold` path and none of them is needed to
ask a question.

Ceremony, **not** cut, and why: step 6, the tool-record compile. The brief
anticipated that if the compile dominates, the errand should not install a tool
record it will not use. Two findings say do not cut it here. First, the compile
is per *effect row*, not per child — the fragment text is
`installTools @(<haskell_effects_alias>) <entry>`
(`resident_workbench.rs:1886-1890`) and the alias is the role's row
(`role.rs:413-422`), so every research child in a run compiles the same text
and the second one onward hits the toolchain compile cache. Second, a child
with no tool record loses `Tidepool.Command` — the shell, the file reads, the
job bindings — which is precisely the surface a read-only errand needs to
answer anything. Removing it would make the errand cheaper and useless. If a
live run later shows this compile is in fact the dominant term for the *first*
research child, the fix is to warm it once at host start, not to remove it from
the child.

## What stage two actually shipped

`errand` in `haskell/actors/Tidepool/Actors/Unfold.hs`, re-exported from
`Tidepool.Actors.Shoal`:

```
errand :: (Member AgentLaunch parent, Member Replies parent, Member Watches parent)
       => Label -> Text -> Eff parent (Watch (Settlement Text))
```

It is `startAgent (readonlyAgent …)`, then `requestWith`, then `watch
(awaitSettled …)` — the three calls a read-only question already needed —
composed into one, on the fresh-child path rather than the fork path. It adds
no launch mechanism: the authority, the parent-owned lifetime and the absent
worktree are what the existing `AgentLaunchWith` capture already produces
(`resident_workbench.rs:3331-3350`, `start.rs:486-492`).

**The reply is `Text`, not a caller-chosen result type, and that is forced by
the extractor.** `requestWith` is a typed-suspension verb head-swapped to its
sited sibling only where both site types are closed
(`haskell/src/Tidepool/SiteClassifier.hs:65-84`). A first attempt kept
`errand @result` polymorphic and marked the wrapper `INLINE`, on the theory
that the occurrence would be rewritten in the authored cell where the caller
had fixed `result`. The extractor rejected it:

```
polymorphic requestWith site in Tidepool.Actors.Unfold.errand: result_a1FW7
```

— the extractor walks the *library's* Core, not only the cell's, so the
occurrence in the wrapper needs a site of its own regardless of what any call
site does. A caller-typed errand therefore needs a new row in `sitedVerbs`
(`haskell/src/Tidepool/EffectSchema.hs:96-155`), which means rebuilding and
redeploying the extractor (`haskell/CLAUDE.md`, "Toolchain resolution and
deployment") and regenerating fixtures. That is a deliberate extractor change,
not ceremony-cutting, and is not in this parcel. `Text` is also the right
default for the errand's actual job: a native subagent returns prose, and the
comparison this wave is trying to win is against a native subagent.

## Kill criterion

If a model still prefers its provider's native subagent for a read-only
question in the next flight, this did not work. The measurement to take is not
a duration: it is the ratio of native-subagent delegations to `errand` calls in
the rollout, the same count that produced six-to-one here.
