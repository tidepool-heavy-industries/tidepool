# Persona quickstart

Use the memory already supplied at arrival when answering what you remember.
No startup inventory is required. The notebook is synchronous; bind evidence
before displaying a selection. A failed read is not an empty memory.

`display value` emits bounded structured output; `display text` emits a `Text`
value. A bare expression only binds an observation for later use. Use `display`
to read a result now, and the available Tidepool effects for actions. Native
`Prelude` IO actions such as `putStrLn` do not run in notebook cells.

## Files and active code

File paths start at your ordinary workspace root. `.exomonad` is an ordinary
editable directory containing configuration, Haskell modules and prompts.
Durable memory remains under `.exomonad/state/memory`; notes remain under
`.exomonad/state/notes`. If listing `.exomonad` fails, report the discrepancy
rather than explaining it away as an intentional state-only view.

`Files.listDirectory "."` lists the workspace after importing
`Tidepool.Effects.Authored` qualified as `Files`.
`Files.readFile ".exomonad/Persona/Spec.hs"` reads its editable draft.
`readSourceModule "Persona.Spec"` inspects the issued generation’s captured bytes
and revision, which may differ from the current active head;
`sourceStatus` compares current drafts with the issued generation. These observations answer different
questions even when the bytes agree.

Import `Persona.Memory` qualified as `Memory` for `Memory.recall Memory.Human`,
`Memory.remember Memory.Human body`, or `Memory.note "name" body`.
`Memory.initialise` creates only absent seed blocks. Inspect each returned result.
Import `Persona.Web` qualified as `PersonaWeb` for
`PersonaWeb.search query 3 [] []`. Search returns source hits, not a generated
answer. Bind a Page returned by `PersonaWeb.open` before using `readLines`,
`findLiteral`, `follow` or optional Jev selection. Required missing credentials
produce explicit failures.

The profile installs files, memory, active-source inspection, Web, Jev, reflection,
forms, context editing, record actors, hosted requests and scoped cleanup.
It omits shell commands and general HTTP. Helpers using `Persona.Spec.helperSpec`
read shared files and return findings; they do not write memory or mount forms.
Use lookup availability and operation receipts for actual runtime admission;
reference material describes a vocabulary, not a grant.

## Lifecycle map

| Operation | Consequence |
| --- | --- |
| Write memory or a note | Durable content changes; current arrival context remains as it was |
| `Persona.Context.refresh` | Reads the current memory files and replaces the published authored context block synchronously; missing or unpublished targets refuse |
| Edit ordinary `.hs` files or workspace prompts | Drafts change; active configuration stays unchanged until reload |
| Native `reload`; Haskell `reloadSource []` | Publishes one coherent source, compatible tool and workspace-prompt generation; changed configuration resets the notebook, unchanged configuration preserves it |

`reloadSource` takes only the list of additional modules to validate. Reload
does not commit files to Git; commit authored changes separately when wanted.

Keep reusable definitions in `.hs` files: notebook names are discarded by changed
reload. Conversation, resources and the published memory block persist. Reload
does not rerun startup or refresh memory; use `Persona.Context.refresh` explicitly.
Already-issued work and existing children retain their captured generation;
fresh-context children select the published source. Captured-only children stay
pinned.

Adding an ordinary helper does not register a native tool. Incompatible registered
tool surfaces or typed actor protocols refuse publication. Preparation failure or
cancellation before commit retains the active generation. Read reload receipts:
a confirmed commit with a projection repair warning is committed; uncertain state
requires reconciliation rather than replay. Source-backed actors can add, edit or
remove modules inside declared roots; required missing imports fail without old
source fallback. Package pins, source roots, external services, capability grants
and root launch configuration remain fixed launch inputs.

`Persona.Orientation.renderMemory` reads the three memory bodies in full, preserving their wording and identifying
their file paths. It does not seed missing files. `Persona.Orientation.startup` supplies that text through
the installed initial-context hook. `Persona.Context.refresh` explicitly rereads
those files and activates their current text. Saving a file does not refresh
context. `Tidepool.Agent.Context.getInitialContext` observes actual publication
and its editable reference; `Persona.Context.activateInitial` can use it to
activate authored text. Read `.exomonad/Persona/Orientation.hs` and
`.exomonad/Persona/Context.hs` for the small editable composition and signatures.

## Exact discovery

`doc <topic>` is query text, not Haskell syntax. In this notebook use
`LookupApi.lookupRaw (LookupApi.lookupRequest ["doc topics"])`, then display the
result. `LookupTools.renderBatch` formats it as text:

```haskell source=persona-lookup-display
import qualified Tidepool.Lookup.Tools as LookupTools
topics <- LookupApi.lookupRaw (LookupApi.lookupRequest ["doc topics"])
display (LookupTools.renderBatch topics)
```

Lookup names such as `Files.readFile` or `Persona.Web.search` for their declarations.

| Question | Lookup query |
| --- | --- |
| Full shared callable examples | `doc api-guide` |
| Notebook publication and reload | `doc workbench`, `doc exomonad-agent-spec` |
| Agent requests, Await and settlement | `doc exomonad-agent-work`, `doc request` |
| Stateful actors and mailbox constraints | `doc exomonad-define-actors` |
| Resource ownership and cleanup receipts | `doc exomonad-cleanup` |
| Semantic judgments and continuations | `doc exomonad-jev` |
| Optional Git delivery and review | `doc exomonad-project-work`, `doc exomonad-review` |

Optional inherited compositions and ancestor suggestions are ordinary files:
`.exomonad/prompts/references/companion-compositions.md` and
`.exomonad/prompts/references/typesafe-compositions.md`. Read them with
`Files.readFile` when useful. They are possibilities, not standing direction.
