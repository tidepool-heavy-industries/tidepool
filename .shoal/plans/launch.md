# Launch and package validation

The launch operator performs these checks once for the selected candidate.
Ordinary implementation workers use the selected package and run product checks
for their assigned changes.

Use a fixed checked Shoal executable. In the application checkout, first run
`shoal check --workspace .` to compile the authored selection without models.
Run `shoal check --workspace . --recipes` to execute the configured package checks
against that candidate. They drive real resident sessions and temporary Git
checkouts without native workers or providers. Compilation alone does not establish
recipe behavior. Starting the paid wave is a subsequent operator action:

```sh
shoal init --workspace /path/to/project --session new-authorized-session
```

Do not recreate an unfinished or unrelated session. All workers use normal Codex
TUIs; talk directly to the owner, lead or specialist for steering. The original
root's .shoal is authoritative. Candidate files in managed checkouts activate only
after checked incorporation there and an explicit next-swarm selection.

## Checking a customization from its own checkout

`[haskell].checks` in config.toml names ordinary Haskell entry points. Keep these
separate from `[haskell].modules`, which are imported into working actors. The
prepared checks live in Project.Checks, Project.CollaborationChecks and
Project.RoutingChecks; their GHCi expressions are adjacent in .shoal/checks.
Edit a helper, its guidance and its checks together, then run:

```sh
shoal check --workspace . --recipes
```

The output names the selected definition identity, entry points and assertions
actually executed. These check coordination recipes in a temporary repository
seeded with authored .shoal files. They create their own source fixtures; they do
not run the application's product tests or prove a live model followed its prompt.
Runtime/log directories are excluded. The candidate source and live swarm stay intact.

Use Tidepool.Check only in those check entry points: root/activation identify exact
resident actors; turn evaluates ordinary GHCi source at its completed tool boundary;
git/readFile/writeFile operate in a check actor's temporary checkout. check asserts
a named fact. present/notPresented/unconfirmed exercise the existing native update
presentation seam. restart deliberately closes the model-free swarm and captures
the changed package; old CheckActor values cannot address the new swarm.

`script actor name` in Project.Checks runs the corresponding .shoal/checks/name.hs
expression file. Most checks are ordinary function calls, Haskell assertions and
small turns over retained values. awaitOutput polls a retained observation while
an automatic callback finishes; it never launches replacement work. No project
role names or stage sequence are encoded in the Rust driver. A new composition
needs a check of its continuation and failure path, not another worker stage.
