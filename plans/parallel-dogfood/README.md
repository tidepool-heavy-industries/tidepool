# Interactive applications and prepared STG: parallel delivery

Status: launch preparation. This is the wave allocation, not implementation
evidence. The human has authorized a Shoal dogfood run for both existing plans.

## Outcome and source of truth

Deliver the full [interactive application plan](../interactive-applications/README.md)
through [A0–A8](../interactive-applications/06-integration.md), and the full
[prepared STG engine plan](../haskell-engine-stg.md) through M0–M7. Accept useful
reviewed intermediate slices, retaining the remaining product gates. Neither a
readback, a scaffold nor the first integrated wave completes either feature.

The detailed plans own semantics and acceptance. This allocation changes their
execution mode: the human explicitly selected parallel dogfooding in place of the
application plan's sequential, non-dogfood operating instruction. Preserve its
dependency edges and matched acceptance. Run on immutable baseline tools; candidate
harness and engine changes do not activate inside the running swarm.

Read root AGENTS.md, this file and the assigned lane index. Leads read the detailed
mechanism sections they own and applicable nested guidance. Leaves receive the
relevant plan section, exact source, shared decisions and consumer obligations;
they need not ingest both complete plans.

## People, models and continuity

The initial Astra planner is the external Codex conversation that prepared this
allocation with the human. It reviews the initial readbacks and consequential
amendments. It is not an addressable Shoal actor: the coordinator publishes the
exact artifacts and requests external planner review through its ordinary TUI.

One Sol coordinator owns combined integration and two substantive Sol leads:

- [Applications](applications.md): full interactive lifetime, delivery, completion
  and recovery, including the Codex fork.
- [Engine](engine.md): prepared STG production cutover, correctness, deletion and
  measured simplification.

Use ordinary Codex TUIs throughout. Leads implement and integrate significant
work themselves, opening recursive Sol subtrees around useful independent
obligations. Bounded implementation and review use Sol Low by default; substantial
leads may use Medium. The declared Astra slots in each lane are for specific hard
decisions. Invoke them when that frontier needs the decision, with a focused
question and evidence; no standing Astra managers or periodic Astra reporting.
Do not kill useful in-flight experts because of an arbitrary token threshold.
Ask the external planner about additional expensive engagements when the need
falls outside these slots.

## First checkpoint: interpret, then release

The coordinator first admits only the two Sol leads with
`childWithProgress @Attention @Delivery`. Each lead keeps Delivery pending and
commits its own-words execution readback under this directory in its checkout.
Use `applications-readback.md` and `engine-readback.md` respectively. Include:

1. A normal consumer journey and an awkward failure case, with concrete owners.
2. The first shared scaffold, its real consumers, and the next ready child tree.
3. Later fork/join frontiers through complete acceptance; identify where the
   earlier result changes the next decomposition.
4. Owned files, shared seams, exact baseline and cross-repository arrangements.
5. Focused checks, unresolved assumptions, challenges and requested decisions.

Publish the committed artifact and cumulative unresolved questions through
Attention. A planning readback is not a terminal product delivery. Do not put a
new request behind the still-pending delivery to send its release.

The coordinator consolidates both readbacks, resolves routine ownership overlaps,
and exposes exact commit/path references plus the coupled choices to the external
planner. Broad implementation waits for that planner's explicit review and
release. Record the accepted corrections in `release.md`, incorporate their source
and send the owning leads active steering through existing supported operations.
Admission, presentation and incorporation remain separate observations. Failed
steering must be reported honestly; the external planner can use the ordinary
owning TUI. An unread artifact or unpresented message is not release.

After release, routine local cycles within that agreement proceed autonomously.
Bring material semantic or scope changes back to the planner in one concrete
batch; unrelated released work keeps moving.

## Haskell workbench use

Use the selected package's `Project.Types`, `Project.Work`, `Project.Plan` and
`Project.Observe`, especially the generic `Task` and `componentLead`. The graph
constructors are an optional example, not this campaign's assignment. The installed
`.shoal/plans/run.md` shows actual invocation, progress and continuation syntax.
No Markdown parser launches workers and this allocation requires no Rust roles.

The initial root resolves the committed baseline with native Git, then binds
`source :: Text` to its exact hash. For example, with checked labels and source:

```haskell
let Right campaign = campaignLabel "interactive-stg"
let Right leads = forkGroupLabel "readbacks"
let group = batch campaign leads
let Right appLabel = branchLabel "applications"
let Right engineLabel = branchLabel "engine"
let app = Task group "plans/parallel-dogfood/applications.md" source "Deliver A0-A8; initial readback and planner release required" "Full interactive applications with precise lifetime and delivery" ["tidepool-agent", "tidepool-node", "tidepool/src/actor_host", "plans/parallel-dogfood/applications-readback.md"] "Accepted A0-A8 evidence at matched revisions; preserve remaining gates in partial slices" []
let engine = Task group "plans/parallel-dogfood/engine.md" source "Deliver M0-M7; initial readback and planner release required" "Prepared STG with a smaller correct production engine" ["haskell", "tidepool-repr", "tidepool-eval", "tidepool-codegen", "tidepool-heap", "plans/parallel-dogfood/engine-readback.md"] "M0-M7 production cutover, semantic checks, deletion and measured cost evidence" []
work <- unfold group ((,) <$> childWithProgress @Attention @Delivery (withEffort Medium (componentLead appLabel app)) <*> childWithProgress @Attention @Delivery (withEffort Medium (componentLead engineLabel engine)))
```

These paths describe the initial principal ownership areas, not exclusive
whole-directory locks or grants to bypass Rust authority. Complete the seam map
below in the readbacks before implementation. Register a result watch and a
progress watch for each returned pair; retain handles, end the turn, and act on
the watch's new evidence. The run guide supplies those expressions.

After each shared scaffold, fork children from its exact incorporated commit.
Use inherited context when the common reasoning is useful; selected context for
divergent tasks and independent review. Do not reconstruct a shared prefix with
long briefs. Leads integrate coherent reviewed slices as they arrive, check the
resulting head, and open the next ready frontier. Keep detail with its owner;
upward packets contain outcome, exact source, decisive checks, unresolved gates
and the next decision.

## Shared ownership and integration

The coordinator owns cross-lane integration and resolves overlaps before edits:

| Seam | Default allocation and handoff |
|---|---|
| Native Codex protocol, queue and completion | Applications lead; one explicit native integration owner and isolated fork checkout |
| Host lifecycle, input, process scope, recovery | Applications; engine supplies changed runtime cleanup/error contract |
| Runtime/session and effect-machine boundary | Divide by concrete mechanism in readbacks; engine owns evaluation representation, applications owns actor recovery consumer; coordinator accepts the shared contract |
| Repr crate | Engine owns execution schema; applications may own specific persistence/version records; reserve exact files and migration numbers |
| Actor/effect schemas and generated bridge | Existing owner retained; changes need named downstream consumers and coordinator agreement |
| Cargo manifests/lock, flake pin and combined package | Coordinator integrates lane proposals; no independent pin races |
| Curated prompt package | Separate prompt author; preserve its work and frozen selection |

Do not serialize unrelated work behind a whole-crate reservation. The owner of a
shared seam must deliver its usable baseline promptly. Native changes use an
explicit isolated checkout of `/home/inanna/dev/codex`, after its AGENTS.md; do
not edit the peer's original checkout. If assigned process authority cannot
access that checkout, report the concrete boundary for planner resolution.

The coordinator owns a compact `status.md`: exact lane heads, accepted slices,
remaining gates and current next owners. This is a checkpoint at real joins, not
an event-by-event ledger. Keep original commits and evidence accessible. Broad
verification belongs at combined integration/release, not every parallel leaf.

## Launch and evidence

Preparation records in `launch.md`: exact committed project seed and native pin;
immutable Shoal/native/extractor paths; selected prompt package identity; focused
package checks; tmux session, run identity and relevant trace locations. Preserve
unrelated dirty source. Every child must be able to read its plans at its seed.

Keep the original-root `.shoal` as the one runtime authority, materialized from
the curated package and captured once. Never reload it or replace the running
compiler/native executable mid-wave. A later candidate launch is a distinct,
explicit swarm boundary; preparation can build and check candidates meanwhile.

Use existing run maps, actor identities, progress, provider usage and opt-in
bounded private traces. Record actual model selection, source/context forks,
message counts, compactions and usage where observable. Missing coverage remains
unknown. Preserve enough operation evidence for a later visualization; do not
create another logger or stream raw histories into the Astra planner. A human
request for RSI can commission a fresh focused Astra sidecar against this evidence.
