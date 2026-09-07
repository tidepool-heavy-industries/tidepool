# A project-specific Shoal workspace

Copy the authored `.shoal` directory into a project and adapt the terminology,
plan paths, acceptance and helpers. TOML is the configuration entry point. Paths
are relative to `.shoal/config.toml`. The normal Codex TUI remains the interface
for every worker. A swarm captures selected inputs once; restart explicitly to
activate edits. No import launches work.

The original workspace root has the swarm’s one authoritative `.shoal`. Every
actor uses its frozen selection, even when a managed checkout contains a copy.
Treat edits in those checkouts as candidates to integrate into the authoritative
directory for the next swarm. Keep these files in the main repository for now;
a nested Git repository is not required.

Astra writes a Markdown plan tree. Each branch identifies its source baseline,
result type, acceptance, recipients and any tagged Astra specialist obligation.
Sol executes the branch with short task-focused contexts. Haskell recipes bind
repeated choices; they are machine coordination code, so use concise ordinary
functions rather than narrative boilerplate or a workflow framework.

A typical chain is:

```haskell
candidate <- implement implementationGroup implementationLabel projectHead task
reviewRoute <- route (awaitSettledFork candidate) $ \settled ->
  case settled of
    ReplyUnavailable failure -> handleUnavailable failure
    ReplyAvailable answer -> do
      reviewed <- reviewCandidate reviewGroup reviewLabel task (responseValue answer)
      _ <- route (awaitSettledFork reviewed) $ \decision ->
        case decision of
          ReplyUnavailable failure -> handleUnavailable failure
          ReplyAvailable result -> case responseValue result of
            Accepted exact -> do
              integrated <- integrateReviewed integrationGroup integrationLabel projectHead exact
              _ <- route (awaitSettledFork integrated) deliverToOwner
              pure ()
            Repair exact defect -> repairExactCandidate exact defect
            NeedsDesign question -> askTaggedSpecialist question
      pure ()
```

The group/label/task bindings and handling functions above are project choices,
not platform exports. Bind them once using the typed labels and request primitives.
A repair is another bounded task against the exact candidate, followed by review;
architectural questions go to the tagged specialist and their result is routed
back to the waiting obligation. Keep callback bodies to scheduling and forwarding.
Do not synchronously wait inside a callback. Retain route handles and inspect
`pollRoute` after failures rather than replaying uncertain effects.

The shipped recipe modules are compiled through the real resident workbench in
`workspace_recipe_modules_and_snapshot_helpers_compile`. Runtime acceptance also
covers typed forwarding, selected-worker launch from a callback, and retained
callback failure. This example does not claim that a model has performed a live
repository integration; reviewers must check candidate and integration evidence.

For an ordinary human-started Astra RSI session:

```haskell
observed <- snapshot
inspectFull observed
inspectFull (swarmUsage observed)
```

Read selected plan branches and outcomes, identify repetitive context or routing
work, then edit prompts/helpers for the next swarm. Keep raw event streams out of
Astra's context unless diagnosing a specific failure. There is no special RSI
lifecycle or budget-enforcement service.
