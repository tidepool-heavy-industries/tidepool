# Authored work plans

`Project.WorkPlan` builds a typed plan with ordinary `do`, `parallel`, and
`Component`. `develop` returns a `Developed` value containing the original
request, progress, receipt, and checked candidate. `review` consumes that exact
value; `integrate` consumes its review proof; `verify` consumes the checked
publication. The coordinator interprets nodes as resident actor work and keeps
the original response handles. `Project.WorkPlanChecks.structural` is the
compiled starting example for composition and the typed parallel join. Each
Review and Verify admission has its own completion ticket, so parallel branches
and repeated reviews of one candidate resume their own continuations. Verify
checks run in a fresh managed checkout of the exact published commit.

In notebook cells, use `import qualified Project.WorkPlan as WP` and
`WP.review`; the default prelude also exports a lens function named `review`.
`Project.WorkPlanRegressionChecks.duplicateReview` compiles this form.

Start `Project.WorkPlanCoordinator.coordinator` with an unbound managed checkout,
the owner, checkpoint tokens whose custody this plan takes, the plan, and an
optional terminal route. The owner calls `beginPlan` once and observes
`planView`. New Luna workers can use `ForkWorker task (lunaWorker label Medium
source)`; `sessionInput :: WorkerAssignment` contains both the Task and the
typed `incorporationRoute`. A branch that needs inherited context can build its
own branch with `withContext (fromCheckpoint seed)` and pass `seed` to the
coordinator's custody list. The runtime fences cross-session seeds.

`answerQuestion` sends an ordinary decision message to the exact pending
worker. Use `correctQuestion` only when an accepted baseline changes that
request's obligation. It validates the current full question, task source,
amendment, decision, and component scope, and calls `updateRequest` on the
original response once. `observeCorrection` records presentation separately;
the worker sends `Incorporation` on its typed route while its request stays
pending. Reported checks are worker evidence, not executed verification. A
refused or uncertain update remains recorded and never starts replacement
work. A corrected development advances only after the update was presented,
the exact incorporation report was accepted without a refusal, and the
submitted candidate descends from the accepted baseline commit.

After terminal outcome, `closePlan` releases owned checkpoint tokens and
finishes routing and join actors. It returns `PlanClosePending` while work is
still active. `viewedReviews` retains exact ReviewFlow handles: the original
owner calls `reviewCleanup` on each terminal flow, inspects its cleanup
receipts, and then finishes that flow. The coordinator cannot impersonate that
owner. Preexisting retained workers need a reporting capability assigned
before their session started; a text message cannot add one to `sessionInput`.
