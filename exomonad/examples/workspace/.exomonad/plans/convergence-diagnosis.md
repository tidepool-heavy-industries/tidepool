# Convergence recipe diagnosis, 2026-09-25

The model-free `Project.ConvergenceChecks.convergence` recipe is not a passing
gate. Three isolated attempts compiled the workspace definitions, created the
retained reviewer and implementer, and committed the first implementer candidate.
None produced a reviewer activation. In the diagnostic attempt, one of the
first two owner calls to `convergenceView` after candidate settlement did not
return during the bounded observation; the forced check immediately after
those calls was never reached. The check runner was stopped deliberately. The
log buffers recipe assertions, so it does not establish the convergence
actor's phase.

The last attempt used a direct collector installation in the request retention
callback in place of a self message. It stalled at the same boundary, so the
self message is not an established cause. That experiment was reverted. The
remaining suspect path is the candidate settlement handler, including source
scope commands, collector installation and review request admission. Isolate
those effects with an actor-local marker or a shorter recipe before claiming
which one blocks. Do not increase the activation timeout to mask this.

Evidence retained in `/tmp/tidepool-convergence-host/target/tidepool-test-runs/`:

- `recipes-20260925T235804Z-2562119/Project.ConvergenceChecks.convergence.log`
- `recipes-20260926T000149Z-2575567/Project.ConvergenceChecks.convergence.log`
- `recipes-20260926T000440Z-2580311/Project.ConvergenceChecks.convergence.log`

The third attempt's worker committed `7296f338c5e95783f238c598f60dcfde0d0f1a21`
in its isolated recipe repository; the reviewer branch remained at the source
commit `cbdb77d7cc39704386d5977a744d2bbf8d0fa9c3`. Source definitions
compiled with fingerprint `9c3ef8c6c1efd7ff3da5690fca65b98bab32b9923a43feacf7724545ba9e324a`.
Acceptance, repair routing, pending question delivery and repair limits remain
unverified by the recipe.
