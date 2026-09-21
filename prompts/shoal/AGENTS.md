# Shipped Shoal prompt sources

This file guides contributors; it is not part of the shipped model prompt.

- `../../tidepool/src/actor_host/prompt_catalog.rs` owns prompt composition and
  catalog identity. `base.md` plus `api-guide.md` form one frozen shared superset
  across roles. Workspace core overrides are selected once at swarm startup;
  never vary the selected prefix by role or live bindings.
- Optimize instructions for model decisions: use established technical vocabulary
  with its actual semantics; explain Shoal-specific departures. Give each contract
  one canonical home, and move rare recovery detail behind targeted discovery.
  Base plus API guide must stay within 2,000 words; inspect the assembled role
  and tool layers too rather than relocating verbosity into always-loaded text.
- Keep core callable signatures and representative examples in `api-guide.md`.
  Avoid ritual startup inventories; recommend targeted discovery only for missing
  information. Check against live/public types rather than inventing API shapes.
- Keep role-specific instructions and runtime authority observations separate.
  Inherited bindings and descriptions do not transfer permissions or reply ownership.
- Teach scaffold, exact-context fork, independent review and checked integration.
  Delivery of a baseline is not acknowledgment or verified incorporation.
- Describe omitted fork effort through the native launch selector's inherited
  default; native Codex goals remain disabled on all Shoal nodes. Verify policy
  against the production selector, not the fallback launch helper.
- Preserve active-update admission/presentation/incorporation distinctions and
  retained failure receipts. Never suggest silently queuing a replacement update.
- Keep `haskell-tool-instructions.md` within its provider size limit. Prompt
  edits must remain compatible with the host's catalog and actual tool surface.

`../../tidepool/src/actor_host/documentation_tests.rs` executes the guide examples
and checks its signature fences. Run its focused
`shared_api_guide_example_handles_success_and_unavailable` test and the prompt
catalog tests when changing shipped guide/composition behavior. Update examples
and owning tests together; unavailable results must not look like success.
