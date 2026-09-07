# Plan understanding before implementation fan-out

The intended order is human + Astra planning, Sol owners explaining/refining their
execution plans, original Astra planner review and steering, then broad Sol
implementation with declared specialists. This closes the gap between delivery
of a plan and evidence that its execution owners understood it.
The initial planner actively interviews the supervising human. The resulting
product plan continues across execution waves until the agreed feature works;
planning, scaffolding and individual wave acceptance are intermediate milestones.

## Adoption tasks

- [x] Make the planning/understanding/review sequence explicit in the swarm vision.
- [x] Require focused human interviewing and a concrete finished user flow in the
  planning guidance, with continuity of product obligations across multiple waves.
- [x] Specify heavily branching recursive execution: each substantial node owns
  repeated local scaffold/fork/integrate waves over exact committed baselines;
  sibling branches advance independently where dependencies permit.
- [x] Request a concrete Sol-authored readback in the current shoal-repl wave before
  opening its four product lanes. Contract and acceptance preparation landed before
  the subsequent operator hold.
- [x] Review that wave's initial owner readback against the intended product and actual public
  capabilities; return corrections, questions and accepted plan improvements.
- [x] Verify the Sol owner records initial planner corrections: application commit
  `027b243` incorporates the review of `a2b3d50`.
- [x] Review all four component plans as a batch against their exact committed
  readbacks. Findings include whole-draft replacement, pending-source access,
  recipient verification and a delayed shared-wiring dependency.
- [ ] After the messaging fix and explicit operator restart authorization, finish
  human clarification, transmit the review corrections, verify their incorporation
  in task packets and release dependent implementation. No implementation approval
  was sent before the hold.
- [ ] Curate the initial Astra planner guidance to interview the human, establish
  feature-level acceptance, and carry a coherent plan across waves of context and
  execution. Keep technical uncertainty separate from a product choice needing
  human steering.
- [ ] Curate the next workspace owner/lead prompts to request plans in the owners'
  own words: examples, interfaces, ownership, fork/dependency tree, model placement,
  acceptance, assumptions, questions and feedback on the original plan.
- [ ] Teach leads to execute successive local waves through fluent Haskell:
  committed scaffold, useful parallel subtrees, checked integration, updated
  context/source and the next scaffold. Preserve the encompassing delivery
  obligation and retain specialists across local waves where useful.
- [ ] Provide a small useful Haskell composition for collecting interpretations
  and delivering planner steering if the live use shows a reusable need. Reuse
  Task, source-bearing decisions, retained requests and progress; do not introduce
  a second workflow registry, Rust role or mandatory procession of actors.
- [ ] Check the changed next-wave package and adopt at an explicit swarm boundary.
  Preserve frozen prompts/modules for useful current workers.

## Review quality

The review tests understanding, not obedience to a paraphrase. Ask whether the
proposed source boundaries implement the desired experience; whether a claimed
missing capability was actually checked; whether acceptance tests the real user
flow; and whether the decomposition creates useful independent work. Sol feedback
can reveal a flaw in the initial Astra plan and should change that plan when
supported. Return a compact concrete decision with affected branches and examples.

Use one initial review of the execution owners, then repeat only for consequential
new shared uncertainty or materially changed scope. Routine implementation, repair
and within-contract decomposition remain with Sol. The planner can become idle
after understanding and corrections are incorporated. Human-requested RSI is a
separate later engagement.

## Current application

Wave: `shoal-repl-live-20260907`, isolated workspace
`/home/inanna/dev/shoal-repl-live-20260907`. The external initial Astra planner
requested `docs/LIVE-WORKBENCH-READBACK.md` from the Sol integration owner. This
request is live operator steering, not a mid-wave shared-definition reload.

Current state: held by the human pending the messaging-substrate fix and an
explicit restart instruction. Root and all four product leads acknowledged the
hold; panes and pending deliveries remain preserved. Integrated application head
is `f6d90983e2ad34b7fed3e0bd0a3884b8c9a7299c`. The four plans were reviewed by the
external Astra, but corrective implementation steering was not sent. Shared
contract and interaction regression are preparation; the requested palette,
selected-agent interaction, relationship UI and animation remain product work.

One concrete review input: the current client graph mirror lacks `creator`, but
the live host's public graph response supplies it. Creation-view implementation
must decode the optional field rather than conclude the capability is absent.
This does not establish a graph-key-to-AgentRef resolution API; that is a separate
capability question. Keep runtime evidence separate from assumptions about source.
