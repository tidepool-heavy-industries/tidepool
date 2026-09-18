# Relation controls and inspector — Sol lead

Input: exact accepted shared-contract commit. Output: `Delivery` with checked UI.
Recipient: Sol integration owner. Dependency: contract incorporation. Read the
shared language and accepted signatures. Implement against that buildable API;
projection can land independently afterward.

Own relation view state in the existing UI owner, `src/ui/agents.rs`, inspector
presentation, canvas integration and their UI tests. Add a clearly labeled,
keyboard-accessible selector for Supervision / Creation / Context. Reuse normal
controls and Iocraft behavior. Preserve F1/F2 navigation, F4 canvas/dense toggle,
editor draft, pending submission and unknown-execution acknowledgment.

Show all three raw relationships in the inspector with exact identities. The
selected view changes display edges only. Keep selection by actor key; reconcile
focus/Back history when switching relations so no hidden invalid focus persists.
Support narrow panes and mouse activation. Explain missing relationship evidence
without claiming that missing fields establish a root or grant permission.

Do not duplicate the projection algorithm inside rendering or mutate supervisor
fields to reuse old layout code. Consume the shared relation contract and retain
raw evidence. Small within-scope refactors are welcome. Send an actual contract
defect to the integration owner, not a synchronous request to a parallel worker.

Check the existing UI/agents, explorer, mouse and live-graph suites as applicable.
Add decisive selection/focus/narrow-layout cases and assert graph controls never
POST Haskell or edit the composer. Use a local scenario/fake server and isolated
terminal capture for visual proof; do not attach to or restart user services.
Update README control documentation. Compilation alone is not UI acceptance.
