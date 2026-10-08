# Shipped Exomonad prompt sources

This file guides contributors; it is not part of the shipped model prompt.

- `../../bridge/facade/src/actor_host/prompt_catalog.rs` owns prompt composition and
  catalog identity. `base.md` plus `api-guide.md` form one frozen shared superset
  for every actor; `agent.md` is one task-neutral developer instruction. Workspace
  core overrides are selected once at run startup, never by actor role or live bindings.
- Optimize instructions for model decisions: use established technical vocabulary
  with its actual semantics; explain Exomonad-specific departures. Give each contract
  one canonical home, and move rare recovery detail behind targeted discovery.
  Use semantic compression: preserve relevant concepts and the conditions that
  change their application. Inspect the assembled base, agent, and tool layers too.
  Record prompt size as an observation, not an arbitrary acceptance ceiling;
  provider-enforced tool limits still apply.
- Shape recognition and method selection: describe dependencies, transformations,
  invariants, and failure mechanisms that transfer across identifiers. Group
  established method cues by relationship, including useful adjacent approaches;
  ground them where ambiguity would change a decision. Target lists and examples
  are starting points, not exhaustive prescriptions.
- Center expressive composition: Kleisli arrows, optics, local algebraic data
  types, typed agent RPC, and small control languages interpreted by stateful
  actors with Jev in their handlers. Teach how parts connect and can be reshaped.
  Types earn their place through useful computation and communication; notebook
  code need not acquire a defensive framework or reusable library first. Give
  lifecycle details where they explain how a composition runs. Preserve concrete
  runtime contracts while keeping type-safety rhetoric out of the motivation.
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
- Keep shared instructions, task requests, and runtime authority observations
  separate. Inherited bindings and descriptions do not transfer permissions or
  reply ownership.
- Keep optional Git project delivery policy in `exomonad-project-work`. The
  shared instructions carry its load cue; general notebook and actor programming
  stays compositional. API skills own their mechanics. Delivery of
  a baseline is not acknowledgment or verified incorporation.
- Recheck cautionary guidance against current source and behavior. Remove obsolete
  workarounds and incident-derived prohibitions; state live constraints through
  the composition they affect and a working way to proceed. Do not preserve an
  old restriction merely because it once prevented a failure.
- Describe omitted model effort through the native launch selector's inherited
  default; native Codex goals remain disabled on all Exomonad nodes. Verify policy
  against the production selector, not the fallback launch helper.
- Preserve active-update admission/presentation/incorporation distinctions and
  retained failure receipts. Never suggest silently queuing a replacement update.
- Keep `haskell-tool-instructions.md` within its provider size limit. Prompt
  edits must remain compatible with the host's catalog and actual tool surface.

Executable guide and reflection fences use `haskell source=<identity>`. Their
authored notebook sources live in
`../../bridge/facade/src/actor_host/fixtures/api-guide/`; `PublishedExample` in
`../../bridge/facade/src/actor_host/documentation_tests.rs` binds each identity to
the source executed by the resident consumer tests. Change the authored source
and published fence together. The source equality gate tolerates reordered
fences and prose edits, and rejects missing, duplicate, or unknown identities.

Run `actor_host::documentation_tests::published_notebook_sources_match_authored_fixtures`,
the affected `published_*` resident consumer tests, and the prompt catalog tests
when changing those examples or prompt composition. Request success, retained
target cancellation, unavailable forms, complete command stdout without replay,
lookup discovery, and unbound reflection have separate consumer assertions.
The canonical `Examples.JevPreparedWorkflow` and `Examples.JevFormWorkflow`
modules own Jev/form composition; their host tests (`JevPreparedTest`,
`FormLifecycleTest`, `FormJevDialogueTest`) and native
`resident_actor::jev_form_runtime_tests` own its success, refusal, and cleanup
coverage. Do not recover examples by fence position or assert tutorial prose
as runtime behavior. Compilation and actual nonzero execution remain separate
qualification obligations.
