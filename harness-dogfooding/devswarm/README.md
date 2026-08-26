# DevSwarm

DevSwarm is the Haskell-native successor to `dev-tree`. One `runLLMTurn`
session owns the root node; its model-authored Haskell dynamically creates
recursive child owners with `fork @OwnerOutcome (renderNodeBrief brief)` and
short-lived repository workers with `delegateTask`.

The ownership tree is not a pre-authored `Plan`, and worktrees are not nodes.
An implementation worktree is an inert candidate returned to its owner for
review or revision.

The current executable slice proves the owner/delegate shape and a real typed
candidate handoff. Two platform capabilities remain before it may advance
repository state durably: a scoped node store and a node-owned integration
workspace. Until those exist, the mandatory selfharness `State` is only a
compatibility seed/last-outcome shell.

Run it with:

```bash
./harness-dogfooding/run.sh harness-dogfooding/devswarm/Harness.hs
```
