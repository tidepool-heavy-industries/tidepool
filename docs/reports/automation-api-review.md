# Automation API review — 2026-09-26

This is an implementation review of the next-wave helper batch, before prompt
exposure. Runtime verification and integration are still in progress. The first
CheckResults recipe passed six assertions through the production command backend,
including late completion, failure, missing evidence, notification policy and
dirty-source reporting; shutdown also completed. Other actor recipes are pending.

## Findings and required changes

| Surface | Finding | Direction |
| --- | --- | --- |
| TestEvidence | An expected count of zero can satisfy the pass predicate with zero selected/executed tests. Reading evidence also ignores the `cat` command's failure if its stdout parses. | Refuse nonpositive expectations, defend the pass predicate, and retain evidence-read execution/cleanup failures. |
| CheckResults | Execution, source identity and strict acceptance are different facts. | Preserve all three. Dirty source must not erase an observed test pass; receipt mismatch must not become trusted evidence. |
| ParallelInvestigate | Only immediately started probes were validated; selected deferred probes could carry invalid directory/memory settings. | Validate the whole admitted selection before launching any command. Keep selected-but-unrun explicit. |
| SlowCommandWatch | Text-only diagnostics discarded output-page loss/refusal metadata; a silent 30-second clamp could alert earlier than requested. | Pass typed bounded pages to the renderer. Refuse unsupported thresholds or expose the actual policy explicitly. |
| AssumptionWatch | Typed fingerprints are retained, but the relevance callback receives only rendered descriptions. | Let relevance inspect typed before/after observations, so code need not parse its own display text. |
| BrowserScenario | Existing `web/dist/index.html` proves presence, not freshness. | Default preparation must build/check, or require an input-matched successful preparation witness. Keep the browser assertion separate to avoid running it twice. |
| Routing | Exact terminal candidate evidence and checkpoint progress are properly distinct; terminal readiness test is still failing. Duplicate input names currently throw `error`. | Resolve the failing terminal test before advertising. Prefer typed setup refusal consistent with CheckResults. |
| Review / Work | Existing automatic review and ordinary task review use different report/decision vocabularies. Retained reviewer reuse does not itself move its checkout. | Reuse one coordinator, share exact candidate/evidence boundaries, retain attempts before effects, and require exact checkout preparation or a fresh reviewer. |
| Diagnostics | Investigate, Reflex and TestEvidence already own investigation, classification and focused evidence. | Compose those owners; do not introduce another classifier, parser or test runner. |
| Interview / handoff | These need bounded collection and supplied facts, not independent actor registries or authority. | Reuse typed responses and existing evidence. Retirement remains a separate operation. |

## Interface rules for this batch

- Use an actor when ongoing events and remembered state justify it. A fixed
  scenario selector, evidence projection or handoff renderer can remain a function.
- Preserve handles before effects that may fail. Diagnostics observe retained jobs;
  they do not silently restart commands.
- Keep typed refusal, missing evidence, running work, failed execution and failed
  cleanup distinct. Compact summaries must retain a route to the underlying facts.
- Prefer small purpose-specific input records where several positional arguments
  are easy to confuse. Avoid a universal workflow configuration type.
- Default displays should be compact; full histories and output remain explicit
  reads. Account for helper compile/effect cost as well as saved model turns.
- The twelve user-facing operations need not become twelve modules or actors.

## Remaining review scope

This pass read the new watcher, probe, browser, focused-evidence and routing
implementations and inspected their Work, Review, Contract, Evidence and Actors
boundaries. It is not a line-by-line correctness audit of all approximately 8,000
lines of existing Project modules. In particular, large legacy Review/Investigate
workflows need focused behavior checks before claims of end-to-end readiness.

Only validated callable examples enter the next-wave prompt menu. Every installed
operation then gets at least three exposed waves; non-use prompts an opportunity
and discovery review before removal. The harness trial inventory remains the
owner of per-wave exposure and outcome records.
