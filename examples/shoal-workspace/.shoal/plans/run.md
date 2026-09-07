# Run and improve this application wave

This package is prepared for `shoal-repl`. Copy/adapt it as ordinary source,
commit it in the app repository, and inspect the exact source commit. Preserve
existing user changes. Runtime artifacts are separate from authored .shoal files.
Use a fixed checked Shoal executable for this application run. First run
`shoal check --workspace /home/inanna/dev/shoal-repl` to compile the authored
selection without models. The same command checks proposed RSI source before
returning its candidate. Starting the application wave is an
explicit operator action, not part of package compilation:

```sh
shoal init --workspace /home/inanna/dev/shoal-repl --session shoal-repl-relations
```

Do not use `--recreate` against an unrelated or unfinished session. Startup
compiles frozen selected modules before replacing a selected existing swarm,
but that does not authorize discarding useful in-flight work. All workers retain
normal Codex TUIs. Talk to the owner, lead or specialist directly for steering.

## Start the contract lane

In the Sol owner's native tools, run `git rev-parse HEAD` at the app integration
checkout. Bind `baseline :: GitRef` to that exact commit using ordinary Haskell.
The following starts the planned contract lead. Watch handles preserve the owned
result; the lead's independent lifetime permits the initial planner to retire.

```haskell
let Right planCampaign = campaignLabel "graph-relations"
let Right leads = forkGroupLabel "leads"
let Right contractLabel = branchLabel "contract"
let Right contractLane = componentLane planCampaign RelationContract baseline
before <- snapshot
contract <- unfold (batch planCampaign leads) (child (withLifetime SwarmOwned (componentLead contractLabel baseline contractLane)))
let Right contractWatch = watchLabel "contract-ready"
contractReady <- watch contractWatch (awaitSettledFork contract)
```

End the turn while waiting. A wake means inspect `pollWatch contractReady` and the
retained settlement. `ReplyUnavailable` is a real unavailable result. A typed
Preparation preserves its holes; a committed candidate still needs integration
and checks before it becomes the next lanes' baseline.

The contract lead reads its assigned Markdown and invokes the declared expert
before the implementation lane. Construct `question :: DesignQuestion` from the
current source and the narrow uncertainty, then:

```haskell
let Right designCampaign = campaignLabel "graph-contract"
let Right designSlot = relationDesign designCampaign
(expert, designReady) <- consultDesign designSlot question
```

Keep the original lead request open. Inspect the answer on the watch wake. A
supported Decision can settle the design choice. AmendPlan is a proposed commit;
after the owning decision accepts it, incorporate and verify that plan change.
If the accepted baseline changes, use it in `implementationSeed` and
`integrationSeed` of the lane you pass to `deliverLane`. Do not reload modules.

A normal lead with satisfied prerequisites installs its whole delivery chain:

```haskell
flow <- deliverLane sessionInput sessionReply
```

A reviewer keeps its review pending while `requestRepair` or
`requestIncorporation` owns a separate request to the retained implementer. End
the turn on a watch, inspect the settled response, review the exact revision,
then return Accepted. A blocking contract question stays with its current owner
until answered or returned as an explicit blocked result. Never synchronously
queue back to a lead already waiting on that review.

## Parallel product lanes

The owner incorporates the accepted contract, runs its focused checks and binds
`acceptedContract :: GitRef` to the resulting exact commit. Both independent
lanes start there. This is an application dependency, not a global wave barrier.

```haskell
let Right productWave = forkGroupLabel "product-leads"
let Right projectionLabel = branchLabel "projection"
let Right controlsLabel = branchLabel "controls"
let Right projectionLane = componentLane planCampaign RelationProjection acceptedContract
let Right controlsLane = componentLane planCampaign RelationControls acceptedContract
(projection, controls) <- unfold (batch planCampaign productWave) ((,) <$> child (withLifetime SwarmOwned (componentLead projectionLabel acceptedContract projectionLane)) <*> child (withLifetime SwarmOwned (componentLead controlsLabel acceptedContract controlsLane)))
let Right projectionWatch = watchLabel "projection-ready"
let Right controlsWatch = watchLabel "controls-ready"
projectionReady <- watch projectionWatch (awaitSettledFork projection)
controlsReady <- watch controlsWatch (awaitSettledFork controls)
```

Each checked lane may integrate independently. Before the final product claim,
verify the combined commit: all-target build/tests, focused new graph/controls
regressions, formatting, and isolated terminal proof using the app's existing
scenario/fake-server tools. Preserve composer state and prove no graph-triggered
POST. Record exact commits, commands, remaining gates and uncertainty.

When a route fails, inspect `listRoutes`, `pollRoute` and retained operations
before intervening. The earlier implementation/review/integration may already
exist. Do not replay the chain blindly or interpret unavailable coordination as
native process death. Keep useful native TUIs available for diagnosis and work.

## Ordinary RSI engagement

When the human requests RSI, the owner takes a fresh snapshot and selected
lane observations. This invokes no summarizing model for routine bookkeeping.

```haskell
projectionEvidence <- observeLane (componentTask RelationProjection) projection
controlsEvidence <- observeLane (componentTask RelationControls) controls
later <- snapshot
inspectFull (usageDelta before later)
inspectFull (usageByRequestedModel later)
```

Bind `source :: Text` to the exact integrated app commit and `question :: Text`
to the human's requested improvement. Point `evidence :: [Text]` at decisive
outcomes/friction artifacts rather than pasting transcripts. For a new sidecar:

```haskell
let packet = RsiInput source question [projectionEvidence, controlsEvidence] before later evidence
let Right improvementWave = forkGroupLabel "requested-improvement"
let Right improvementLabel = branchLabel "workspace-style"
improvement <- unfold (batch planCampaign improvementWave) (child (withLifetime SwarmOwned (rsiBranch improvementLabel (atRef (GitRef source)) packet)))
let Right improvementWatch = watchLabel "improvement-ready"
improvementReady <- watch improvementWatch (awaitSettledFork improvement)
```

The packet correlates plan paths, exact actor identities, definition identities,
owned delivery state and creation-tree observations. An invisible owner is marked
explicitly. Usage deltas separate comparable spend, newly observed history and
counter/source discontinuities. Requested model groups are not billing totals.
If the sidecar needs live high-level observation, `shareObservation (forkedActor
improvement) (forkedActor projection)` grants that scope without stop authority.

RSI returns a normal Candidate for .shoal changes. Review/check and incorporate
it into the authoritative original-root .shoal. Finish or deliberately hand off
in-flight work, then explicitly start the next swarm to use those definitions.
Check the next worker's `previewLaunch`/workspace identity and selected behavior.
Editing candidate source during a wave does not change its frozen core.

The first run should leave useful app changes and a checked customization
improvement, plus honest evidence about guidance gaps. Deterministic package
checks establish wiring; fresh-model usability and real usage savings require
this actual application run. No comparative scientific evaluation is required.
