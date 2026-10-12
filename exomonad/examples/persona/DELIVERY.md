# Companion seed delivery

Implementation work is isolated from main and from the ongoing conversation and
recovery work. No running service has been redeployed.

## Prompting follow-up

Branch `companion/persona-metaprompt` adds prompt/configuration/documentation
changes on the preserved seed commit
`afb3526e27de72a76bddafb1398ac839a6ccb4f5`. Its persona-specific base uses the
existing workspace override; the host still appends the shared API guide.
The agent prompt seeds embodied curiosity, ambitious Haskell composition,
interaction through Jev and GUI, and engineering in the spirit of customizing
one's own Emacs. Astra/System 2 and Jev/System 1 frame an editable hybrid
cognition; the companion may locate its sense of self across that ensemble and
steer its recursive self-improvement with human assistance. The base embeds
verbatim MIT-licensed TypeSafe guidance with attribution and the license in the
prompt directory. Ancestor notes are explicitly optional. Activity begins
through human interaction; no autonomous scheduler is introduced.

The native and browser results below belong to the original implementation.
This follow-up adds no Haskell snippets or executable changes. Its checks cover
TOML parsing, referenced prompt/module files, local documentation links, patch
scope and whitespace. Prompt composition was inspected against the existing
production selection path. No live behavioral trial or release qualification
has run for the revised seed. Research and deferred runtime work are retained in
[PROVENANCE.md](PROVENANCE.md) and [NEXT-IDEAS.md](NEXT-IDEAS.md).

## Source and integration

- Tidepool base: `2dd63765c0ea3a8c4c9f556cac292eb994848189`.
- Harness base: `8bc63f8815458102c0114a8d1dc5507b19fd2ee4`.
- Required renderer feature commit: `10974a671a28065bf033a13ca8c81cd8658e84f7`,
  branch `companion/persona-renderer-reconciled`.
- Tidepool feature branch: `companion/persona-harness-main`.

Tidepool's Cargo and browser pins remain at main's selected
`26fb5665d50a38cceabfdc9ec85ac0558a9ab7b8`. The live config is unchanged.
The new `slider` and `alternatives_many` form variants require the renderer
feature. This is a coordinated integration handoff, not a runnable matched
release: use the repository's dependency-update workflow to align Cargo
resolution, generated Buck inputs and `harnessWeb` with the coordinator's
selected Harness integration commit. Then configure, build, freeze and qualify
that exact bundle. Do not independently merge the active Harness histories.

Native additions are limited to the Tavily Web effect, scoped existing file
handlers under `.exomonad/state`, and active-source reading. Actor, subagent,
context-editing, scheduling and conversation mechanisms are existing owners.
The prepared-display test now uses the production handler-stack constructor so
it compiles with those additions. The approved existing form fixes cover
initial-selection uniqueness, integer seed syntax and `__proto__` draft keys.
The Haskell test roster retains main’s `ActorExit` and `ActorProfiles` modules.
The renderer uses the reconciliation checkpoint’s generated DTOs and validators;
slider fields use its existing numeric schema owner.

## Executed checks

All native commands ran as inanna through `swarm-build`, using provisioned
checkout-specific bind mounts and local Buck execution with remote disabled.
Logs are retained under `/srv/swarm/evidence/inanna/persona-seed-20261011/`.
The new-base acceptance manifest records exact commands, source patches,
commits, execution counts, exit status and artifact hashes.

- Persona, existing form/Jev and form lifecycle selection: 20 passed
  (`main-persona-haskell.log`).
- Existing form-view selection: 25 passed (`main-form-view.log`).
- Tavily request policy and evidence identity: two passed (`main-web-handler.log`).
- Active-source read and affected prepared-display fixture: two passed
  (`main-source-display.log`).
- Harness slider/selected-branch validation: two passed
  (`reconciled-harness-forms.log`).
- Browser suite: 318 executed, 312 passed, six failed. All 16 mounted-form
  cases and six generated-contract cases passed (`reconciled-browser-final.log`).
  The new controls also pass the generated stored-form validator.
- Browser typecheck and production asset build failed on errors in unchanged
  checkpoint files (`reconciled-browser-checks-3.log`). The six failing tests are in `UnifiedChat`, `WorkerChat`
  and `actor-output`. These belong to the ongoing reconciliation; this feature
  does not repair chat/transport. `reconciliation-browser-findings.json` records
  the unchanged-source comparison. The entire browser suite is not passing.

`acceptance-original-bases.json` retains earlier passing results and original
feature commits separately; those results do not qualify this port. Native
formatting differences in unchanged Tidepool `exomonad.rs` tests match the base
exactly and are left with their owner. Harness formatting passes under its
Rust 2024 edition; Tidepool feature changes use its Rust 2021 edition.

## Qualification boundaries

There has been no live Tavily/Jev/persona session or frozen-bundle qualification.
Unit tests establish mechanics, not the companion's emerging behavior or actual
provider cache savings. Checkpoint-scoped source reads explicitly return
`SourceUnavailable`; root/run-source inspection is implemented and tested.
Any extension of that source owner belongs with its mainline maintainer.

The root is configured as `gpt-6-astra`, medium. Helper model aliases are Luna
and Sol, with Astra available for contextual forks. The root-model boundary is
prompted policy: the existing context effect also has model-selection operations.
The seed adds no new runtime restriction or pre-inference context-assembly hook.
Persistent code changes remain operator-integrated proposals; notebook behavior,
working context and durable memories can evolve through the installed effects.
