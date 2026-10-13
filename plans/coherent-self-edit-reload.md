# Coherent self-edit reload

Accepted implementation contract, 2026-10-13. Coordinator branch:
`persona/selfedit-delivery-20261013`, starting at `1a6fdf943c`.
Current main `46aeb2d7ab` is an ancestor. Preserve the live Persona and all
foreign WIP. Supersedes the unqualified parser/namespace-overlay experiments.

## User-visible behavior

Ordinary files hold reusable Haskell definitions, workspace prompts, and memory.
One explicit `reload` activates edited source, compatible AgentSpec handlers,
workspace prompts, and a fresh notebook. Haskell `reloadSource [additionalModules]`
adapts to that same operation. Reload does not commit files to Git; ordinary Git
operations own version-control publication. Remove separate helper/spec publication implementations and
migrate callers; compatibility shims are not required.

Changed configuration resets notebook names; unchanged configuration does not.
Failed preparation or cancellation before commit preserves the active generation.
Conversation, published memory context, actor identity, and owned resources persist.
Reload does not rerun initialization or implicitly refresh memory. Prompt-only
changes count as changed configuration. Memory contents do not enter that key.

Existing requests and children retain their issued generation. Fresh-context
children select the published source. Explicit adoption of parent state is deferred;
captured-only children remain frozen. Git/rebase/reload applies to source-backed
actors with an editable checkout, not to captured runtime state.

Registered tool surface and typed actor protocol compatibility remain required.
Source roots, package pins, service configuration and capabilities are launch inputs.
New recovery formats may refuse old runs; old live runs retain their matching binary.

## Shared contracts

An opaque actor-owned `ActorGeneration` retains a generation ID, complete immutable
source graph, installed tool lease, composed prompt snapshot, and notebook scope
lease/publication owner. Only prepared components can construct it. Actor/session/
resource identity is independent from notebook scope. Requests capture the whole
generation once; nested work, source inspection and completion preserve that owner.
Never reselect the current generation midway through an issued request.

Runtime recovery owns typed generation-qualified public ownership and active
generation selection. Reuse RunRecoveryStore/PreparedRecoveryWrite and its native
installation acknowledgement; do not add a journal or independent head registry.
Persist exact source, installation and prompt references, not display labels.
The actor layer retains live leases; runtime serialization must not depend on the
actor crate. On restart, the existing incarnation-transfer transaction transfers
the actual active generation and its exact references, rather than assuming
generation 1. This startup transfer is distinct from reload and keeps its existing
incarnation invalidation semantics. Runtime/source/actor owners agree their concrete handoff before edits
to shared signatures; report changes to the coordinator and dependent workers.

Publication: durably prepare immutable inputs and isolated scope; fence expected
generation; claim PublicationDecision; commit generation/head atomically; install
the already prepared live generation; acknowledge native installation. Source
symlinks/status files are derived projections repaired from durable state. An
uncertain commit blocks admission and reconciles; it never authorizes blind replay.
Late A publication targets A, never B. Release old scopes only after leases end.

The notebook generation ID is nonzero, initially 1, and the durable owner issues
successors. Initial captured-child admission may retain inherited state; only
reload replacements must have an isolated empty notebook. Do not apply the
replacement emptiness rule to ordinary captured-child startup.

Staged source exposes immutable prepared inputs, not a second commit operation.
The active actor generation and durable head provide the stale-writer fence;
derived symlinks cannot supply a conflicting authority. A projection repair
failure after a confirmed transaction reports the committed generation and a
repair warning. It is not a rollback or evidence of database uncertainty.
Unrelated domain revision changes may rebase the prepared immutable write when
the expected active generation remains unchanged; they must not repeat compilation.

## Source and compiler

Cut old workspace-source fallback and restrictions justified only by it. A selected
revision contains the complete workspace/helper graph plus explicit immutable
shared libraries and runtime resources. Add/edit/remove modules inside declared
roots; removed modules cannot resurrect from an older source tree or cache catalog.
Missing required imports fail normally. Keep reserved module ownership checks.

One existing native installer candidate compilation validates configured, changed/
new, helper and also_check modules and prepares handlers. Use validated imports in
the existing builder/cache key. Remove the preceding duplicate check_source pass.
Candidate preparation consumes the full staged graph, including helpers. Builtin
readiness must not bypass changed-source validation.

Installer originals remain private to its installation scope. Notebook B is minted
with mint_isolated_scope and retained through the existing scope lease; never copy
notebook A imports/facades/projections or publish the whole installer closure into B.
Normal notebook compilation acquires source originals lazily. Preserve exact typed
request evidence and runtime contract imports; conflicting nominal seals refuse.
Keep compiler/machine/cache owners and declaration allocation counters alive.

## Prompt publication

Compose captured workspace base/agent/API-guide text with host-owned invariants.
Use the same ActorGeneration snapshot for instructions and tool/source authority.
Harness request_snapshot precedes instruction interning and request construction;
transport and replay use identical captured instruction bytes. Retries retain them.
Compaction uses the same selection rule. EngineConfig.instructions remains only
the generic-client fallback. Do not rewrite conversation history or use user-message
context editing as an instruction publication mechanism. Preserve cache affinity.

## Parallel owners and acceptance

- Actor owner: generation issuance, isolated notebook scope, unified reload dispatch,
  late completion, lifecycle and lease cleanup.
- Runtime owner: generation-aware durable ownership, one recovery transaction,
  recovery/fault injection/properties. Coordinate types with actor owner first.
- Source owner: complete source graph, staged capture/prompt inputs, one compile,
  derived source projections; coordinate shared staging API with actor owner.
- Prompt owner: Harness snapshot instructions/replay/compaction and facade adapter;
  independently commit and pin the reviewed Harness result.
- Acceptance owner: actual self-edit history and display-page oracle; source edits
  through production APIs only. Release qualification follows joined acceptance.

Each worker owns an isolated checkout and compiles affected production and test
consumers before delivery. Admit parallel builds against actual headroom; at start
host has ~50 GiB available and build.slice uses ~54 GiB of its 96 GiB maximum.
Preserve scope evidence, exact commands/counts/revisions. No worker merges or deploys.

Required histories: edit/reload/use; changed vs unchanged; new/deleted modules with
warm caches; invalid source and incompatible contracts; A issued then B published
then A completed; old generated module import refusal; pinned children; prompt A/B
across inference, retries and compaction; crash before/after commit; cancellation;
resource and memory/context preservation. Independent shrinking operation-history
models cover issuance/publication/recovery without running GHC for every case.

Repair the joined display assertion using its retained output owner, not aggregate
textSource output or substring trimming. Preserve previously qualified persona
parcels. Exclude overlay/parser WIP and unqualified debug instrumentation.

Run the joined production history early. Then qualify one exact matched bundle,
smoke with Sol 6.1 medium, and launch replacement Persona with Astra medium while
preserving authored files. Lookup optimization remains a separate background task.
