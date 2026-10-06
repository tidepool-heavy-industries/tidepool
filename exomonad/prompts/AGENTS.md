# Shipped Exomonad prompt sources

This file guides contributors; it is not part of the shipped model prompt.

- `../../bridge/facade/src/actor_host/prompt_catalog.rs` owns prompt composition and
  catalog identity. `base.md` plus `api-guide.md` form one frozen shared superset
  across roles. Workspace core overrides are selected once at swarm startup;
  never vary the selected prefix by role or live bindings.
- Optimize instructions for model decisions: use established technical vocabulary
  with its actual semantics; explain Exomonad-specific departures. Give each contract
  one canonical home, and move rare recovery detail behind targeted discovery.
  Use semantic compression: preserve relevant concepts and the conditions that
  change their application. Inspect the assembled role and tool layers too.
  Record prompt size as an observation, not an arbitrary acceptance ceiling;
  provider-enforced tool limits still apply.
- Shape recognition and method selection: describe dependencies, transformations,
  invariants, and failure mechanisms that transfer across identifiers. Group
  established method cues by relationship, including useful adjacent approaches;
  ground them where ambiguity would change a decision. Target lists and examples
  are starting points, not exhaustive prescriptions.
- Define acceptance through inspectable artifacts. Separate exploratory scope
  from the handoff, observations from inferences, and product failures from
  failures of the investigation. Explain what quiet results can establish and
  how to check sensitivity. Keep task-specific authority and acceptance with the
  assignment rather than inventing universal gates.
- Review wording counterfactually: what recognition, hypothesis, analogy, or
  decision would be lost if a clause disappeared? Cut duplication and unrelated
  associations; retain distinct useful referents. Walk through a representative
  case, an analogous case with different vocabulary, and a case where the method
  does not apply. These walkthroughs support editorial hypotheses; execution
  trials must judge decisions and artifacts, not expected words or headings.
- Keep core callable signatures and representative examples in `api-guide.md`.
  Avoid ritual startup inventories; recommend targeted discovery only for missing
  information. Check against live/public types rather than inventing API shapes.
- Keep role-specific instructions and runtime authority observations separate.
  Inherited bindings and descriptions do not transfer permissions or reply ownership.
- Teach recursive scaffold, ready parallel unfold, independent review and checked
  integration as the execution workflow. Sol owns cross-component choices; Luna
  owners recursively delegate to justified terminal leaves. Reviewers do not
  create another review tree.
  Delivery of a baseline is not acknowledgment or verified incorporation.
- Describe omitted fork effort through the native launch selector's inherited
  default; native Codex goals remain disabled on all Exomonad nodes. Verify policy
  against the production selector, not the fallback launch helper.
- Preserve active-update admission/presentation/incorporation distinctions and
  retained failure receipts. Never suggest silently queuing a replacement update.
- Keep `haskell-tool-instructions.md` within its provider size limit. Prompt
  edits must remain compatible with the host's catalog and actual tool surface.

`../../bridge/facade/src/actor_host/documentation_tests.rs` executes the guide examples
and checks its signature fences. Run its focused
`shared_api_guide_example_handles_success_and_unavailable` test and the prompt
catalog tests when changing shipped guide/composition behavior. Update examples
and owning tests together; unavailable results must not look like success.
