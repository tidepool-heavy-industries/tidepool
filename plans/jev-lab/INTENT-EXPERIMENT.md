# The same failed build under three statements of intent

The interview established that the Sol agent's disagreement with the report came
from its assignment, not from anything it read in the source:

> The decisive evidence was: **your assignment**: the commit "adds a limit
> parameter to store::load." That establishes the signature change as
> intentional.

So the program was rebuilt around that. Two changes:

**The repair strategy is now the question, and the location questions are
conditional on its answer.** A failure like a changed signature admits two whole
repairs: bring the callers up to date, or put the signature back. They are not
independent per-location judgments. Asking each location "must this be edited?"
without fixing the strategy produced answers near a half, which the consumer
correctly distrusted. The strategy is now one `Choice`, accepted under
`J.merging` because a repair gets acted on.

**Once the strategy is fixed, the obligations are mechanical.** Under "callers
catch up", every site the compiler reported is an obligation, with nothing left
to judge. Under "restore the definition", none of them is. The model is asked
about a location only when the compiler did not report it.

`investigate` gained an `intent` parameter: statements about what the change was
for. Nothing else changed.

## The result

Identical failed build (`53ad43c`, four wrong-arity errors), identical owned
paths (`src/store.rs`), identical requirements. Only `intent` differs.

### No intent supplied

> two repairs fit this failure and nothing said which was meant, so this report
> does not choose between them. Both are set out below.
>
> under the repair that puts the definition back, edit:
>   - src/store.rs:59
>
> under the repair that brings the callers up to date, edit:
>   - src/main.rs:83, src/store.rs:81, src/store.rs:100, src/store.rs:110

No obligations, no ownership request, both repairs named. Previously this case
produced a confident list that changed between runs.

### "the commit deliberately adds a limit parameter to store::load"

> the change at the definition was meant, so the reported sites have to catch up
> with it
>   accepted: confidence 1.00, mass 1.00, margin 1.00
>
> must be edited:
>   - src/main.rs:83, src/store.rs:81, src/store.rs:100, src/store.rs:110
>
> requires an ownership decision, which this investigation does not make:
>   proposed addition to the owned paths: src/main.rs

This is exactly the answer the Sol agent reached on its own, including the
escalation of `src/main.rs:83` to its parent.

### "store::load is a published API and its existing signature must be preserved"

> the change at src/store.rs:59 was not meant, so putting it back is the repair
>   accepted: confidence 0.99, mass 0.99, margin 0.98
>
> must be edited:
>   - src/store.rs:59

One obligation, inside the owned paths, so no ownership request at all.

## What this settles

The program does not need better thresholds. It needed a fact it was never
given. Intent is not recoverable from diagnostics, and no amount of probability
tuning substitutes for it. Supplied as one sentence, it moves the strategy from
refusal to near-certainty in both directions, and the rest of the report follows
mechanically.

That makes conversational or assignment context a concrete first consumer for
whatever supplies it: the input is one or two sentences, and the effect on the
output is total.

## Also fixed here

Omitted locations now carry their addresses rather than a count. The consumer
had said of the count:

> I did not hunt for opaque hidden entries through the module. I independently
> searched every `load` call. I treated the report's lists as incomplete.

A count warns and does not help; an address list does both.

## Known wording defect, not fixed

When the strategy is unclear, the report prints the acceptance explanation line
underneath, which reads "accepted: confidence 0.88 ≥ 0.85, mass 0.92 ≥ 0.70".
That is the explanation for accepting the "insufficient evidence" alternative,
and sitting under a heading that says nothing was decided it reads as though
something was. It should say the model was confident that the evidence is
insufficient.
