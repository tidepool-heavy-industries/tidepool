# The agent spec and its System 1 slots

Each checkout carries one Haskell module that defines an agent's tools and its
System 1 slots. The agent edits it with ordinary file tools and asks for a
reload. Answers `~/dev/tidepool-astra/plans/agent-spec.md` and the interview that
followed it.

The editable source is the development interface. Retained compiled functions are
the execution mechanism. Nothing compiles per tool call or per turn.

## Why

A tool whose schema never changes but whose implementation acquires
context-sensitive retrieval, Jev judgments and prepared follow-ups is already a
large change in what an agent can do. That is the whole of this design; dynamic
schemas are not part of it.

Four motives, in the order they carry weight:

1. **System 1.** Routine semantic judgments are answered by Jev inside tools and
   slots, so the frontier model is interrupted only for the unresolved case.
2. **After-tool helpers.** A slot runs when a tool finishes and attaches useful
   evidence to that result, selected against the agent's recent turns. This
   makes the after-tool evidence-pruning pattern standing, rather than a cell
   the model must remember to write.
3. **More slots follow.** After-tool is the first. Because
   slots fire often, they are precompiled.
4. **A kit that compounds.** The module is workspace source, so the next session
   inherits it.

## Shape

    the actor's checkout                .shoal/AgentSpec.hs
      → explicit reload                 (never on file save)
      → one compile                     declarations + retained handlers
      → invoked at supported events     tool calls, and later triggers

A reload is two distinct acts, and conflating them was the one real error in the
original spec:

- **publishing a source revision** — the checkout's source layer moves;
- **activating a spec** — one actor swaps the retained values it dispatches on.

An actor that has not activated a published revision is still running the old
one, truthfully, and says so.

## The spec is one module among the ones the agent curates

It is not a special location. `.shoal/AgentSpec.hs` sits beside `.shoal/Project/`,
in the same declared source roots a notebook cell imports, and a reload publishes
all of them in one revision. The notebook and the spec are two consumers of that
one revision, activating on their own schedule: the next cell compiles against it
when it runs, and the actor re-derives its record when it asks.

That is what makes the promotion path cheap. A helper written in a cell moves to
`.shoal/discoveries/` while it is experimental, to `.shoal/Project/` when a second
consumer genuinely shares it, and becomes a tool or a slot by being *named in the
spec* — no copy, no move, no second library location. The same module can back a
cell today and a tool tomorrow, compiled once from one revision, so the two can
never disagree about what the helper does.

The reverse direction matters as much: a tool's implementation is ordinary source
the agent can import into a cell and exercise directly, without going through the
tool boundary to test it.

## What the spec value is

The existing tools DSL, unchanged, plus one record around it. A tools record is
already an ordinary `Generic` record whose fields are endpoints
(`haskell/lib/Tidepool/Agent/Contract.hs`, the `mode :- endpoint` family at
`:118-135`; `haskell/lib/Tidepool/Command/Tools.hs:78-85` is a shipped example).
A field's name is the tool's name and its input and output types generate the
schemas, so no tool is ever named by a convention.

The spec keeps that property for slots. They are fields of one record, not
magically-named top-level functions:

```haskell
agentSpec = defaultSpec
  { tools     = Tools.definitions
  , afterTool = Just AfterTool.run
  }
```

**The record needs a default, and that is the point.** More slots are expected,
and a slot added as a new field with a default leaves every existing spec
compiling untouched. A spec written as a bare constructor application would break
on every addition, so the default is what makes the surface extensible rather
than versioned.

So the whole convention is two names: the module `AgentSpec` and the value
`agentSpec`. Everything a model writes below that is ordinary Haskell it can read
the type of.

## How the spec is found

By convention, because configuration cannot express it. `[haskell] tools`
(`tidepool/src/shoal/workspace.rs:24`, wired at
`tidepool/src/actor_host.rs:2046-2051`) is one workspace-global key resolved once
at composition-root construction and threaded identically into every actor. It
names one entry point for the whole run, which is exactly what a spec per
checkout is not.

An actor resolves its spec in this order, and stops at the first that exists:

1. `AgentSpec.agentSpec` in the actor's own checkout, if `AgentSpec.hs` is
   present in a declared source root;
2. the workspace's `[haskell] spec` key, when a workspace wants another name;
3. the existing `[haskell] tools` entry point;
4. the built-in default.

In the shipped example workspace `[haskell] source_roots = ["."]` under `.shoal`,
so `.shoal/AgentSpec.hs` is module `AgentSpec` with no new path resolution and no
new source root. A checkout without the file behaves exactly as today, so
adopting a spec is adding one file.

Two obligations follow from discovery being implicit:

- **The resolved path and module are reported.** Status and the reload receipt
  name which of the four rules matched and the file it came from. A model must
  never have to guess which spec is live, and a spec that was not found because
  it sits outside a declared source root must say so rather than silently
  falling through to the default.
- **The discovered module joins the checked closure.** The reload's typecheck
  covers everything reachable from the configured module list plus the driver
  (`tidepool/src/shoal/source.rs`). A spec found by convention is not in
  that list, so the reload adds it, and a spec that fails to compile fails its
  own reload instead of surfacing later at an unrelated call.

One file per checkout, one `agentSpec` export. A worktree agent has its own
checkout and therefore its own spec; the root's checkout holds the root's. No
role dimension inside the file, because the checkout already supplies it.

## What the existing code already guarantees

`prepare_tools` (`tidepool-actor/src/resident_workbench.rs:1864-1982`) compiles
one fragment, reads the declared schemas out of the `AgentToolsInstallWith`
suspension it produces, and retains the parked continuation as an
`Arc<RootCustody>`. Declarations and dispatcher are two products of one compile,
so a schema can never advertise a handler from another revision. A call clones
that `Arc` (`tidepool-actor/src/resident_actor.rs:4555-4592`), so a call already
accepted keeps its implementation with no further mechanism.

`SourceLayer` (`tidepool/src/shoal/source.rs`) captures source by content
identity, typechecks a candidate, publishes by one `rename(2)` of a symlink, and
returns a rejection as a value.

Reflect resolves with the executing actor and nothing else
(`resident_actor.rs:2104-2124`) and excludes only the turn currently executing
(`tidepool-agent/src/interactive.rs:630-634`). A slot running in actor A's
resident machine therefore reads A's own completed turns, including the one that
just ended.

One handler runs at a time per actor, structurally
(`haskell/lib/Tidepool/Event.hs:240-248`,
`haskell/lib/Tidepool/Actor.hs:378-391`).

## The four gaps

1. The tool record installs once behind `policy_installed`
   (`resident_actor.rs` ~3616) and `compiled_tools` is never reassigned.
2. The source layer is one per run and root-only, so a child in a worktree has
   no layer to reload.
3. The Codex bridge registers tools once and serves them read-only
   (`tidepool/src/host_dynamic_tools.rs:1-7`, `:190-252`).
4. Nothing diffs two sets of declarations. Prompt fingerprints answer equality
   only (`tidepool-actor/src/prompt_catalog.rs:65-75`).

## Reload

1. Reload the actor's own checkout layer. A failed typecheck ends here as
   `ReloadRejected`: edited files stay on disk, the previous graph stays active,
   and the receipt names both revisions and carries the diagnostics.
2. Compile the install fragment against the new revision.
3. Compare new declarations against active ones by name, description, input
   schema, output schema, kind and order. **Any difference refuses the reload**
   and returns the diff. The old record stays active.
4. Otherwise swap the `Arc` between calls.

Because the spec shares source roots with the modules a cell imports, these steps
can end in different places, and the receipt says which. Step 1 publishing while
step 2 fails is a real outcome, not an error: the revision is live for later
cells, and this actor is still serving the previous record. A model repairing its
spec can therefore still import and exercise the new modules from a cell while
the spec itself does not yet compile.

Gap 3 is why step 3 refuses rather than asks. The bridge cannot accept a changed
tool list mid-session, so no confirmation flag could make one work; a schema
change takes effect at the actor's next incarnation. The declaration comparison
is what makes `Tidepool.Agent.Contract`'s standing requirement — that the
declared surface stay stable — true by construction rather than by convention.

Reload is scoped to the actor that asked. It never upgrades children. A child
reloads the same snapshot itself if it wants it.

## Slots

A slot is a retained function the runtime applies at a supported event. Slots do
not share one input and output contract; they share System 1 modules.

**After-tool**, the first slot. At the tool-result boundary, after the result
exists and before it is returned, apply the retained slot to the call and its
result in the actor's own resident machine.

    NoAnnotation | Abstain Reason | Annotate Text | Pruned Text Handle

- **Annotate or prune, never rewrite.** A pruned view says it is a selection and
  the full result stays addressable by its handle. The annotation is attributed
  as derived context, distinct from the tool's own output, so an observation is
  never mistaken for a judgment.
- **Finding nothing to say is silent.** A slot that judges a result unprunable,
  or simply finds nothing worth adding, returns the original unchanged and
  records its reason in the receipt. It does not announce a refusal. The model
  never asked for a judgment on this result, and honesty does not require every
  internal non-decision to occupy its context. A slot that *failed* rather than
  abstained is different: the original is preserved and a concise warning appears
  where the failure could change how the result is read.
- **The result waits for the slot**, up to five minutes. The point is not to
  spend an inference before the evidence arrives: a result delivered early with
  its annotation following could send the model investigating, or acting, just
  before the thing it needed appears. Past thirty seconds the elapsed time is
  reported through the existing progress path, which never wakes the model.
  There is deliberately no second late-delivery route in the first cut. If
  waiting proves routinely wasteful, that is a policy to revisit once use shows
  it.
- **On timeout or failure the original result is delivered**, with one compact
  failure line and a reference. Work the slot already completed is preserved
  rather than silently replayed, effects are not rolled back, and a repeated
  failure does not fill the conversation with copies of one diagnostic.
- **No recursion.** A slot's own effects and tool use never trigger a slot, and
  the reload and status tools are never annotated, so a broken slot cannot block
  its own repair.
- Each annotation records the source revision the slot was compiled from.

**After-turn** is deferred. There is no protocol notification to hang it on: the
`turn/completed` JSON-RPC notification (`process.rs:811-827`) is read only while a
caller is blocked inside the headless one-shot seam, and the interactive path
never spawns that subprocess at all. This is a Codex-only question by
construction, since `CodexInteractiveBackend`
(`tidepool-agent/src/backend/codex/node.rs:270`) is the sole interactive backend.

There is deliberately no rollout-tail lifecycle detector. Native steering now
crosses the generation-bound input-control protocol, whose owner reports
not-submitted separately from unconfirmed submission and never polls rollout
text as transport authority. A future after-turn feature needs an explicit
owned notification and durable acceptance boundary; it must not revive the
removed rollout-tailing path.

## What an invocation records

Dispatch clones a retained tool record, so the identity of *that record* is known
at the moment of the call and costs nothing to carry. A completed tool call and a
slot invocation both record it: the installed record and the source revision it
was built from.

That is the honest claim and the limit of it. It says which installed record
served the call. It does not say that every function reachable through that call
belongs to one revision, which would be a different and possibly false claim. The
distinction is the same honesty limit the source reload work already observed,
and reopening comprehensive closure provenance is not part of this.

It lives in inspectable receipts, not in repeated model-facing text. If the
narrow identity turns out to need real new tracking rather than a field carried
from install, it is deferred — better absent than stamped with whichever revision
happened to be active when the call finished.

## Not part of this

- Automatic reload on file save.
- Changing a tool schema or description within a live actor.
- A second scheduler, registry or tool protocol.
- Automatic migration of changed state types.
- Replacing a pinned dependency without a declared override.
- A mandatory catalogue of reflexes. The authored program decides how much
  retrieval, judgment and action is useful.
