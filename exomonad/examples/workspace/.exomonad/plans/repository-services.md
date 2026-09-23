# Repository services: live screenshot candidates

Helpers live in `Project.Search`, `Project.History`, `Project.Service`, and
`Project.Repository`. They were typechecked with `reloadSource` and exercised
in the resident workbench. No frontier-model workers were launched.

## 1. Semantic grep

```haskell
import qualified Project.Search as Search
docs <- Search.load ["README.md", "CONTRIBUTING.md", "ARCHITECTURE.md"]
obsoleteSetup <- Search.grep "instructions to launch Tidepool as an MCP server over stdio" docs
Search.render obsoleteSetup
```

Observed: `CONTRIBUTING.md:37` and `:39`, including the obsolete
`cargo install --path tidepool` / stdio MCP launch instructions.
The documents and line-addressed passages remain in `docs` for later queries.

## 2. Turn a closure into a resident service

```haskell
import qualified Project.Service as Service
index <- R.start $ Service.serve $ \query ->
  Search.render <$> Search.grep query docs
R.call (Service.ask (R.client index)) "passing functions between agents"
```

Observed: the actor searched captured document values and retained its query
and answer in a private record. Results included the relevant README paragraphs
and some loose matches. `Search.grep` is a semantic shortlist, not exact retrieval.

## 3. Upgrade the running service

```haskell
import qualified Project.Repository as Repo
scout <- R.replace index (Service.serve (Repo.answer docs))
R.call (Service.ask (R.client scout))
  "Which commit fixed the State import needed by retained Handler values?"
```

The final live run used the fifth replacement handle, `scoutV5`, and returned:

```text
[history]
Compared 2 candidate patches.

commit e1fb071e42fd3e8b8706120b2e0c45f97d5ebd12
Author: Inanna Malick <inanna@recursion.wtf>

    fix(workbench): expose actor state effect qualification

 bridge/facade/src/actor_host.rs | 1 +
 1 file changed, 1 insertion(+)
```

Jev selected history rather than documentation. The history reader gathered
Git summaries, found the semantic choice uncertain, read candidate patches,
and selected the actual implementation. The continuation ran its own follow-up
commands without an intervening reasoning-model turn.

The same service also routed a documentation question to the retained corpus
and a child-source isolation question to commit `b03350e34`.

## What the experiment changed

- Titles alone were insufficient for the import-fix question. Adding summaries
  still left it uncertain. The helper now reads up to three candidate patches
  before asking again; it does not lower the acceptance policy.
- Feeding full patches through the record actor exhausted the resident engine's
  observation budget of 100000. Patch evidence is now bounded to 4000 characters
  per candidate, explicitly marked when partial, with exact Git SHAs retained
  for deeper reads. The bounded continuation ran successfully inside the actor.
- A failed actor handler could not be drained until replaced. Replacement
  recovered it with the previously committed record intact.
- The final record contained six completed query/answer pairs, spanning policy
  replacements. The failed handler did not append a record.
- Jev results can vary. The transcript above is one actual run, not canned output.

`scoutV5` remains live for screenshot capture. The two earlier history-only
actors were finished. Finish the remaining service with `R.finish scoutV5`
after capture. No repository implementation files were changed by these demos.
