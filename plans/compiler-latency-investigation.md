# Compiler latency investigation

Measure the joined engine before changing compilation policy. The retained
104-second cell ran on `2ca69b0`; it is historical evidence, not a benchmark of
the joined candidate. Its main compiler request spent 42 seconds preparing a
text binding, 17 seconds preparing a quoted command, and 15 seconds preparing
four displays. The first binding prepared products for 72 modules after the
candidate offer exceeded its four MiB metadata budget.

## Owners and experiments

| Owner | Question | Controlled comparison |
|---|---|---|
| Transport/cache admission | Which limits or proof gates discard usable candidates? | Bounded metadata plus independently hashed graph parts; first and warm offers; missing/corrupt/oversized parts |
| Compiler preparation | Why does a target require complete support bodies? | Greeting alone, complete saved cell, warm repeats; complete ownership evidence versus demanded executable products |
| Displays | Which work is repeated just to present retained values? | Binding without display, one display, repeated displays, function display |
| Product certification | Which products and validations are genuinely fresh? | Candidate selection, serialization, host admission; repeated decoding/hashing and request-local validation |
| Allocation/GC | Which functions allocate and retain the expensive data? | Matched optimized vanilla and standard GHC cost-centre workers; finite `A,A,B,A` cohort |
| Metrics/review | Are stages and evidence correctly attributed? | Preparation stages versus actual requests; exclusive profile totals and single-count queue intervals |

The core baseline joins the main source with the generic-product eligibility
and literal-producer fixes. A second candidate adds descriptor transport.
Record exact revisions and matched binary identities in experiment evidence.
New workers must generate their own scopes and artifacts; a changed producer
cannot reuse another producer's authenticated compilation inputs.

## Resource and evidence policy

One expensive build, test, or experiment runs at a time. Source analysis can
proceed in parallel. The coordinator grants each admitted unit and records its
memory limits, owned processes, output paths, and result. Preserve live runs
and shared services. Profiling overhead is not production latency.

Retain commands, source and artifact hashes, actual compiler flags, inputs,
request/admission identities, profiles, cache decisions, exit status, and
cleanup. Separate startup, first use, and warm repeats. Allocation counters,
live heap, RSS, GC CPU, nested phase times, and elapsed time are distinct.

## Decisions after measurement

Prefer eliminating dropped offers, repeated validation, and unchanged body
preparation. Preserve exact private identities, interface/body correspondence,
hidden-instance and family isolation, original quotation execution, freshness
fences, publication, and no replay of effects. A raw optimization-tier switch
does not prove these properties.

Every proposed repair needs its owning boundary, measured cost, correctness
argument, and a focused before/after check. Delete prose-only test assertions;
retain compiled examples, identity/replay contracts, UTF-8 byte budgets,
cleanup, and cache-identity checks. M2 qualification and deployment proceed
independently of performance improvements.
