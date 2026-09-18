# Interview: a Sol agent that used Project.Investigate on a real task

Session `invtest`, `gpt-5.6-sol` at medium effort, 2026-09-17. It was given the
`53ad43c` failure, told it owned `src/store.rs` and nothing else, told the
module existed and where its documentation was, and asked to diagnose rather
than edit. It used the module. Answers below are verbatim.

## Did it help?

> **New information versus raw output:** No essential information. Reading the
> compiler diagnostics plus git grep was enough. Its coverage summary and
> separation of related tests were convenient.

> Without the module, I would miss only convenience: automatic grouping of
> diagnostics, a quick list of candidate locations, a reminder to consider
> ownership and search coverage. I would miss no essential diagnostic insight.
> Raw compiler output, git show, and git grep were more reliable here.

## What was wrong with it

> Unclear/misleading: the probability routing contradicted explicit compiler
> errors. It treated obvious broken call sites as uncertain and included noisy
> search matches. I had to verify everything against the raw output and git grep.
>
> Missing: correct argument values, the application-level limit policy, and a
> clear ownership escalation for `src/main.rs`.

On the undecided band:

> I did not trust the "undecided" classification. I checked each location.
> [...] The section did not improve my answer.

On the coverage line that counted omitted locations:

> Yes, I noticed the "3 examined locations" line. I did not hunt for opaque
> hidden entries through the module. I independently searched every `load` call
> at 53ad43c. I treated the report's lists as incomplete.

That is the count working as a warning and failing as a retrieval aid. A count
tells a reader something is missing and gives them no way to get it, so they
redo the whole search.

## Whether the re-reads were verification or habit

> Both, but mainly verification. I would still have read the exact diff and
> nearby source. I would not have repeated the broad caller search if the report
> had been reliable and exposed convincing evidence. Its probabilistic
> misclassification forced the independent git grep.

So a coherent report would have saved the broad search. The re-reads were a cost
the incoherence imposed, not a cost of the pattern.

## What settled the disagreement

The report put the definition on one side and the call sites on the other; the
agent concluded the opposite. Asked what evidence made it sure:

> The decisive evidence was:
> - **Your assignment:** the commit "adds a limit parameter to store::load."
>   That establishes the signature change as intentional.
> - The commit diff: line 59 deliberately adds `limit`, and lines 61-62
>   implement the cap with `.take(limit)`.
> - The compiler errors: every failure says an existing call supplies one
>   argument when the intentional signature now requires two.
> - Ownership: my file includes three stale test callers; `main.rs:83` is the
>   same stale-caller problem outside my ownership.
>
> The commit message supported this, but was not necessary. I did not infer the
> conclusion from an unseen source file or merely guess the author's intent.

This is the whole finding. The agent did not know something about the code that
the module had failed to read. It knew what the change was *for*, and the module
was never told. The question the module was asking of each location, "must this
be edited", has no answer until the repair strategy is fixed, and the repair
strategy is a question about intent that no amount of reading the diagnostics
can settle.

It also noticed the instability without being told about it:

> The report I saw actually put `src/store.rs:59` under "leave alone", not
> "must be edited". If another run put it under "must be edited", that run was
> wrong.

Two runs of identical input had landed on opposite sides of the floor.
