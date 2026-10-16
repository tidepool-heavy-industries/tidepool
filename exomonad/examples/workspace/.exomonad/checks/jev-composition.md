These are editable examples; they do not install a selection or memory policy.
`Examples.JevComposedWorkflow` is their shared compiled module. Its header lists
extensions and its signatures list effects. The selected workspace source path
must contain that module, `Project.EvidencePattern`, `Project.Lookup`,
`Project.ConversationContext`, `Jev.Operators`, and `Jev.Recording`, together with
the workspace's pinned `Jev.Core` source root and the matched runtime library.
Installed lookup bundles include the authored source prerequisites.

Run each line or marked stage in an ordinary resident Haskell notebook cell.
The notebook preamble supplies `Text`, `J`, `display`, overloaded labels, strings
and record dot. For an authored module, use the imports/extensions from the
shared module and `Control.Monad.Freer (Eff, Member)`; effect types come from
`Tidepool.Effects.Core`. Standard GHC typechecking cannot execute native JSON
intrinsics. The installed examples' successful decoding and replay require the
matched Tidepool runtime. Compilation alone is not runtime acceptance.

`jev-batch-example.hs` needs `Jev` and `Console`. It batches two independent
questions over equal evidence into one request, retains the complete Evaluation,
projects all choice masses with original typed rows (including zero masses),
compares two caller-defined policies, and executes a continuation only at the
explicit final stage. Change its evidence, actions and thresholds for your task.
Real observation needs a configured Jev provider and credentials; inspection,
policy comparison, reconstruction and replay need no credentials or new calls.

`keyedDistribution` uses exhaustive handlers for heterogeneous static alternatives
and a runtime candidate family. Its handlers are deliberately reordered; labels
connect them to the original offers. Each reported mass stays beside its key and
original typed payload, including zero masses in authored order. It does not
filter to contenders or normalize. `fullDistribution` keeps the same payloads
without keys. `distributionMetadata` projects keys, masses, source addresses and
excerpts into ordinary data without demanding or executing their carried actions.

The final cell binds `evidenceFor sourceIdentity`. Supply the exact selected
source/module identity from the source owner when retaining its result. The
resulting `decisionEvidence` envelope includes the original request input and
raw outcomes through `Jev.Recording`, the complete metadata distribution,
evaluation failure when present, and the separately observed continuation result.
A zero-record archive can reflect preparation failure; inspect `evaluation_failure`
as well. Keep the original Evaluation for typed failures and live payloads.
Serialize this ordinary envelope with the normal JSON/filesystem operations when
needed; never serialize functions or treat a selected action as executed. The
caller owns the destination and publication guarantee, just as for the archive
recipe below.

`jev-lookup-example.hs` additionally needs `Lookup` and `Reflect`. Capture retains
the original candidate pool, packing omissions, complete ranking Evaluation and
bounded exact view-fenced expansion. Reassessment is a dependent request over
bounded declaration text and structural capture evidence, with explicit
truncation flags and primary/expansion receipts. The returned capture remains
complete even if deciding evidence is truncated. Refusal, selection failure,
successful empty selection, confident insufficiency and policy doubt remain
inspectable separately. A confidence policy cannot supply omitted evidence.

For persistence, run `jev-recording-save-example.hs` with `Jev`, `FsWrite` and
`Console`. It writes only ordinary recording data to the named scratch file;
direct `writeFile` success does not establish durable publication. Retain the
original Evaluation separately if you need preparation failures or its result.
Edit a continuation payload in `Examples/JevComposedWorkflow.hs`, publish the
changed source through the ordinary source reload workflow, then run
`jev-recording-replay-example.hs` with `FsRead` and `Console`. Reconstructing a
fresh program is necessary: a previously bound function still owns its original
captured source. Replay has no `Jev` requirement and makes zero provider calls.
Payload-only edits are allowed; changed model, question or evidence produces a
typed replay refusal. Occurrence metadata is retained for inspection; exact
request equality authorizes replay. The pure controls demonstrate this without
replacing the normal source reload operation.
