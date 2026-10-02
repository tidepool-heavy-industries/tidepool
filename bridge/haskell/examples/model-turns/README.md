# Small model turns inside Haskell

Use ordinary Haskell for deterministic work and Jev for a compact semantic
choice. Use `Tidepool.Model` when a bounded piece of work needs several tool
calls and a generated answer, while the surrounding Haskell program keeps
ownership of the workflow. Use an actor when it needs an ongoing mailbox,
independent lifecycle or delegated implementation.

`Coordination.hs` is the executable source of four seeds: route a change from
actual diff evidence, clarify a dependency with its owner, prepare an evidenced
handoff, and inspect retained failure output. Supply project-specific callbacks;
only those tools are available. Keep important authority in the callback owner.
The examples deliberately expose no shell or rerun operation.

Build a reusable `textTurn` or `typedTurn`, then call `invokeModel turn input`.
Tool records, argument/result schemas and optional after-tool hooks are the same
`AgentSpec` surface. This initial adapter accepts JSON function tools with object
inputs; raw custom tools and scalar inputs are refused before provider admission.
Use small records for arguments. A typed result may contain nullable fields,
numbers and disjoint tagged alternatives. Unsupported schemas fail preflight.

Match `modelOutcome`, retain `modelReceipt`, and handle a budget failure as an
ordinary result before running cleanup. The invocation receipt contains evidence
references and measured usage; it does not flood the notebook with a transcript.
A hook may prune only the exact retained output handle it receives.

The effect is opt-in and requires the admitted cell service. These seeds are not
advertised in the always-loaded prompts until that engine hook passes joined
acceptance; see `plans/model-call-integration-handoff.md`. They have native Haskell
compile coverage; native contract tests execute callbacks in the caller effect
row. This is separate from resident/JIT and live-provider acceptance.

## Editing an agent's retained context

`Tidepool.Agent.Context` provides an authority-free `Context` value for
reviewing and curating the current actor transcript. The runtime retains
authority: a reference in this value can identify a transcript item, but it
cannot open or restore one. `editableTexts` focuses authored text blocks,
while `visibleTexts`, `blockKind`, and `blockProvenance` let a curator inspect
what a proposed edit contains and where it came from. `toNotes` turns only
selected completed exchanges into authored notes and preserves each exchange
reference as note provenance; it leaves pending and protected native groups
untouched.

Context reads and writes require `ContextReadWrite`. A regular asynchronous
compiled tool does not receive that effect. Use an explicitly synchronous
tool or synchronous Haskell cell when the actor must commit context changes
before the invocation settles. This makes `unfoldDeferred` useful for a
parent-curates-then-delegates workflow: the parent can store its edit and
finish the invocation, after which its actor-owned children inherit the
committed transcript and the same Haskell bindings. The child can choose the
model for its next request with `setNextModel "executor"`. The argument is
`Text`: the host resolves a configured alias first, then treats an unmatched
value as a literal model identifier.

The returned `Context` has a bounded structural display, so a direct
`getContext` or `modifyContext` cell result shows authored blocks and safe
native previews. `ContextWorkflow.inspectAndCurate` demonstrates reading
completed-exchange provenance, turning those exchanges into notes with
`toNotes`, and composing that conversion with the ordinary `editableTexts`
traversal. The example module is checked with the model-turn fixtures.

Keep curation in ordinary Haskell. For a compact semantic choice, `J.each`
can examine packets while the author retains the exact original text and
applies only selected original slices. The model's judgment is a selection;
it does not rewrite source text or confer access to transcript references.
