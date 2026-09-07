# Relation projection — Sol lead

Input: the exact accepted shared-contract commit. Output: `Delivery` with checked
projection. Recipient: Sol integration owner. Dependency: contract incorporation,
not merely the contract worker saying it is done. Read the shared language and
contract notes at this input commit. Sol Low is the default; local decomposition
is optional when independent implementation work warrants it.

Own the graph projection and traversal in `src/agents.rs` and focused pure tests.
Use the accepted selector/parent API. Implement one relation-aware forest owner
rather than three copied algorithms. Every supplied actor must remain visible
exactly once; actor order should be stable under irrelevant server reordering.
Handle cycles and absent parents according to the accepted contract. Do not
silently change raw creator, supervisor or context evidence to make a tree fit.

Keep the existing supervisor projection available and correct during incremental
integration. Coordinate a concrete interface correction with the integration
owner if the scaffold is insufficient; do not queue on the parallel controls
worker or add adapters around an unresolved ownership defect. The controls owner
owns UI event bindings, canvas wiring and inspector presentation.

Commission independent review of exact owning consumers. Repair locally and reuse
the reviewer; with delegated implementation, retain its worker for direct repairs. Check
reorder/insert/remove, self and multi-node cycles, orphans, multiple independent
roots and same-id/different-incarnation fixtures. Run the owning unit tests and
compile all changed targets. This partial result may integrate before controls;
final visible relation switching remains a product gate until the UI lands.
