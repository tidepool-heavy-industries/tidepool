# Installed workspace surface

`.exomonad/config.toml` selects the modules, spec and project prompts for this
repository. `.exomonad/workspace` pins the shared Haskell package. Run
`exomonad check --workspace .` to validate that selection before a new run;
a file on disk alone does not install it in a running actor.

## Execution workflow

Use [the recursive-work procedure](../workspace/RECURSIVE-WORK.md): scaffold,
admit the ready parallel batch, integrate checked children and repeat locally.
The Sol root owns shared decisions; Luna owners delegate through useful
subcomponents to justified terminal leaves. `lunaLead` requests Delivery;
`lunaTask` preserves the caller's actual result type. `unfoldWork` retains the
original handles and owns event collection. Use `currentCheckout` for the local
owner's scaffold; `projectHead` explicitly selects the root project source.

`startReviewFlow` composes counted checks, exact-source review and bounded repair
for a separate completed implementer. Manual `requestReview` admits the revised
candidate's exact checkout. Pending review questions need the question-only
collector shown in the review skill. Accepted review, incorporation, post-merge
verification and resource release remain separate facts.

`Exomonad.Contrib.Merge` owns checked publication. A failed integration/check retains its
source and evidence for the owner; it does not silently roll back. Bounded
`DecisionAnswers` actors relay supplied original owner decisions and escalate
new, stale, conflicting or uncertain cases. The default AgentSpec installs the
typed tool records without a blanket watchdog.

## Discovery and examples

The shipped base/API guide and role prompts describe the standing workflow.
Project role instructions come from the configured `prompts.files` mapping.
Use targeted `lookup` for public signatures, and the skills under `.agents/skills`
for executable examples. They link to the pinned shared workspace in this checkout.

The pinned shared workspace and `exomonad/examples/workspace/.exomonad` carry
the same release source; the facade scaffolder selects that exact revision. This guide does not certify all recipes green. Exact executed gates
and remaining limitations are recorded in the current implementation report and
run launch record. Older `.exomonad/plans/examples` are retained reference
experiments, not the canonical execution procedure.
