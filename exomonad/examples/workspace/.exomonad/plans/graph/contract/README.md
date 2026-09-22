# Shared relation contract — Sol lead

Input: exact initial app commit containing this plan package. Output: `Delivery`
with the checked shared-contract commit. Recipient: the Sol integration owner.
Dependencies: none. Read `../../language.md` and `design.md`; this component
unblocks both projection and controls.

First use `relationDesign` and `consultDesign` for the declared Astra question:
how to project all three relationships without fabricating ownership or losing
actors under missing parents/cycles. Supply this plan, the exact source, concrete
current `AgentNode`/`AgentSnapshot` consumers and alternatives. Keep this request
open while the expert answers. Within this plan's contract you may accept a
supported decision. Incorporate any proposed plan commit and verify it before
using it as the child baseline. Structural changes go to the human in this TUI.

Own `src/graph_wire.rs`, the minimal shared changes in `src/agents.rs`, fixture
constructor updates across the app, and the contract document. Establish a
small typed relation selector and a real raw-parent projection for each variant.
Add optional creator input compatible with snapshots that omit the field. Keep
unknown creator separate from supervisor/context. Preserve exact incarnation.
Compile every existing consumer; do not install a fake successful graph algorithm
or add a new transport/service. No new dependency should be necessary.

Scaffold the interfaces that projection and controls will consume, with real
minimal behavior and unchanged default supervisor UI. The algorithm belongs to
projection; view state/control behavior belongs to controls. Document signatures,
source owners and semantics in this plan's committed contract notes before both
lanes fork. Do not freeze a speculative API solely to maximize parallelism.

Implement directly and use independent OwnerRepairs review; repair locally and
reuse that reviewer. Delegate only a useful independent implementation frontier. Relevant starting checks:
`cargo test --test graph_protocol`, affected `agents`/wire unit tests, and
`cargo test --all-targets --no-run`. Distinguish old-field decoding, explicit null,
known creator, absent referenced parent and distinct incarnations. A buildable
partial contract is useful; selectable graph UI remains an explicit later gate.
