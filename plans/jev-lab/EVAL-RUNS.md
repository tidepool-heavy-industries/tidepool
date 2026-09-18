# Two monitored runs, fed small exercises

2026-09-17 evening. Two live Shoal sessions, each a `gpt-5.6-sol` lead at low
effort, fed one small exercise at a time from the operator's seat and read
afterwards from their rollouts. No feature was attempted. The point was to find
out what fights a model that uses this harness honestly.

- **`evalA`** — one lead, no children, six exercises on the seat itself:
  discovery, notebook state, redeclaration, a first Jev call, adapting a lab
  example, and writing a question of its own.
- **`evalB`** — one lead and three Luna leaves in a separate clone of the toy
  repo (the workspace lock is per path), each leaf owning one file with the whole
  job of adding one passing unit test, then a mid-flight correction, then
  collection, cleanup and integration.

Both workspaces were preloaded with the lab's strongest cells and its two
guidance documents under `.shoal/jev-examples/`.

## What works, stated plainly

**Today's two notebook commits are confirmed by a model that did not know they
were recent.** Asked to declare a function, redeclare it, and end a cell with
`pure` applied to a value, the `evalA` lead reported: *"Nothing was rejected,
including on first attempts."* Receipts read `defined summarize at generation 1`
then `generation 2`.

**The discovery commit is better than `VERIFIED.md` credits it.** That file
records it as finding that a module exists but not how to call it. On the first
exercise the lead asked `doc topics`, read the line the change added —
*"Workspace modules (compiled into this session; lookup a name to browse it)"* —
went straight to a six-name lookup, and got 349 lines of real declarations back,
answering with six exact types including
`mergeInto :: WorktreeId -> Maybe BranchName -> ActorSpec Merge MergeEffects`.
What it still does not give is a worked invocation.

(That signature is quoted as the lead saw it. `mergeInto` has since gained a
third argument, the check command, so that it cannot silently run a
workspace-wide suite; `.shoal/plans/INSTALLED.md` carries the current one.)

**Jev works cold, and the numbers are right.** Asked to adapt whichever lab
example asks several questions in one request, the lead picked
`12-termination.hs`, kept its shape, rewrote every question for this
repository's own failing check, and ran it in one attempt in 71 seconds with
nothing rejected. All four answers verified afterwards against the fixture:

| question | answer | truth |
|---|---|---|
| kind of failure | `non_exhaustive_patterns`, mass 0.99, confidence 0.98 | five `error[E0004]` non-exhaustive pattern errors |
| names `app::ActivePanel::Tags` as uncovered | 0.98 | named 15 times |
| locates it in both `src/panels/status.rs` and `src/main.rs` | 0.97 | both named |
| exactly five error sites | 0.97 | exactly five |

Every alternative in its choice described what the state field contains, which
is the rule the lab spent a day learning. It got that from the example without
being told it was a rule.

**`RECOGNIZING-FIT.md` transfers.** Asked to write a question of its own and
defend it, the lead rejected *"Does HEAD contain a source file dedicated to
tags?"* under the document's first test, because tracked paths answer it
mechanically, and asked a reader-simulation question instead. Its own reading of
the 0.61 that came back: *"only a perspective for prioritization, not an
established repository property."* That is the distinction the document works
hardest to draw.

**Cleanup is fixed.** Run 7 left four actors unretired. `evalB` planned and
executed a group retirement that stopped and forgot all three children and
returned `cleanupReceiptComplete = True`. See the caveat below.

## The record actor starts

Run 7's decisive blocker was that `R.start (gateFor …)` could not be started at
all: the prepared engine rejected it with *"prepared compilation rejected:
unsupported expression at node 18541 in binding ValueId(17401)"*, because the
emitter had no arm for GHC's rubbish atom. Every level of that run then did
review and merge coordination by hand, in notebook turns, which is the thing
these actors exist to avoid. `70e4ff22e` fixed the emitter and is an ancestor of
this tree, but nothing had started a real record actor since.

**It starts.** Told the history and asked to try without working around a
failure, the `evalA` lead started `Project.Merge` as actor `9@1`, submitted HEAD
itself as the smallest possible candidate, and got a complete cycle:

```
Published (GitOid "65bbb44…") (GitOid "65bbb44…") check.sh=ok(review-run)
```

with the actor's own recorded state:

```
merge tree=…/wt-0370577c-… advance=- blocked=no
[0] integrate 65bbb44 review-run merged_green: record-actor-startup-exercise:
    AlreadyContained (…); green -> Proceed (Green) -> replied to the review
```

It then finished cleanly. The lead's closing line: *"No errors or
prepared-compilation rejections occurred."*

So the actor starts, takes a publish request, merges, runs `check.sh`, reads the
result, decides, replies to the review and retires. Run 8 is not blocked on the
engine.

**What this does not show.** The candidate was HEAD, so the merge was
`AlreadyContained` and the check was green. The red path, the rollback path and
the Jev seams inside the record actor were not exercised by this. What is
established is that the actor compiles, starts, and completes a cycle — which is
exactly what run 7 could not get.

## The bugs

### 1. A child can report success while its work is never committed

Two of the three `evalB` leaves replied that they had added a test and run it
green — *"Added one unit test, `help_panel_title_is_help`, to
`src/panels/help.rs` only. `cargo test panels::help::tests` passed: 5 passed,
0 failed."* — and their branches are still at the base commit. Only
`status-test` committed anything.

```
help-test    65bbb44  (base)
status-test  50e4781  test status panel title
list-test    65bbb44  (base)
```

The real work exists, as uncommitted modifications in worktrees that outlived the
group:

```
wt-c9062e09…   M src/panels/help.rs
wt-8cbc3fd4…   M src/panels/list.rs
```

The parent received `ResponseReady` carrying a prose claim and no candidate
commit, and nothing between the child's reply and the parent's acceptance
compared the two. When it later merged the help candidate it got
`Right (AlreadyContained …)` — the honest answer to a merge of a branch that
never moved, arriving long after the point where it could have mattered.

Note what is *not* the problem: the children did as they were told. The parent's
assignment asked for a test and a summary and never mentioned committing; one
child inferred it and two did not. The gap is that the harness treats a child's
prose as a settlement without ever asking whether a candidate exists.

### 2. ~~`cleanupReceiptComplete = True` over abandoned work~~ — retracted

**This was wrong, and I am leaving it here rather than deleting it.** I reported
that a complete cleanup receipt over two dirty worktrees was a defect. It is the
documented contract working as designed. `doc cleanup` says so in as many words:

> Cleanup never deletes worktrees, branches, commits, build evidence, or user
> files. Dirty worktrees remain available after actor retirement.

Checked against the receipt rather than assumed: every actor reported
`AgentStoppedNow`, which per the same document means both phases completed — the
actor published its terminal state *and* the host released its process, pane,
tool service, socket and workspace view. The runtime resources were released. The
directories remaining is the promise being kept, not broken.

**What survives is narrower and still worth fixing: retained work is not
discoverable from the receipt.** A lead reading `cleanupReceiptComplete = True`
learns that the group retired. It learns nothing about two worktrees that still
hold uncommitted changes, where they are, or that they are deliberately being
kept. The contract preserves the work precisely so a person can come back for it;
the receipt gives them no way to know there is anything to come back for. A
receipt that named the retained worktrees would close that, and nothing would
need to be deleted.

The general lesson is the one this whole document keeps hitting from different
sides: I read a directory on disk and inferred a failure without checking the
contract that governs it. That is the same mistake as reading a rollout and
inferring intent.

### 3. `updateRequest` reports success for an update nobody will receive

Told mid-flight that whoever owns `src/panels/status.rs` must use a specific
test name, the `evalB` lead did the right thing — `updateRequest` on exactly the
one child's retained response, then a separate poll — and got:

```
(Right (RequestUpdate (RequestId 2) 1),
 Right UpdateTooLate)
```

The first value is what a caller sees from the send. The update had already been
classified undeliverable when it was queued. Only the second, separate call
reveals that nobody will ever see it. Confirmed against the child transcripts:
the string `status_line_names_the_active_filter` appears nowhere in any child,
and the status owner used its own name.

The fix site is `tidepool-actor/src/request/updates.rs:282`. `update_request`
computes deliverability, pushes either `UpdatePhase::Queued` or
`UpdatePhase::TooLate`, and returns `Ok` with `delivery = None` in the second
case. Two lines above, `TargetState::Reserved` and `TargetState::Queued` already
`return Err(ReplyError::Stale)` — the function knows how to refuse an update it
cannot deliver, and simply does not refuse this one.

This is run 7's "steering does not trickle down" with a mechanism attached.

### 4. Discovery advertises constructors the notebook refuses

Browse answered `data WorkerContext input = Inherited | Selected (input -> Text)`.
The lead wrote `withContext (Selected id)` and was rejected:

```
<cell>:8:23-31: error:
    Data constructor not in scope:
      Selected :: (a0 -> a0) -> WorkerContext input
```

`haskell/actors/Tidepool/Actors/Unfold.hs:37` exports `WorkerContext`
abstractly — the name alone, not `WorkerContext(..)` — with smart constructors
`inherited` and `selected` beside it at lines 308 and 311. Hiding the
constructors is deliberate. Browse reads the declaration rather than the export
list and prints them anyway, and the rejection does not name the smart
constructor that would have worked. A direct lookup of `Selected` in the same
turn returned `no match`, so the type view and the symbol view disagree, and the
misleading one is the type view.

**Located, not yet fixed.** `browseEntries`
(`haskell/src/Tidepool/Introspection.hs:731`) renders every entry with
`pprTyThingInContext showEverything thing`. `showEverything` is what prints all
of a `TyThing`'s constructors and fields regardless of the export list; GHCi's
own `:browse` abbreviates precisely here. The export list is already in hand two
lines above — `inspectModule` passes `modInfoExports` in as `names` and
`browseEntries` binds it as `exportedNames`, using it only to decide whether an
entry has an exported parent. The fix is to render sub-things against that set.

It is a GHC-plugin change needing an extractor redeploy, so it is recorded here
rather than started late in a session with other live work on the machine.

### 5. `apply_patch` reports a file updated when nothing changed

**Not ours.** `apply_patch` is a Codex tool, not part of this harness. Recorded
because it hurt an agent working inside a Shoal run and because anyone reading a
child's transcript should know a patch receipt can claim a file was updated when
the tree is unchanged. Nothing here can fix it; a child can defend itself by
running `git diff` after a patch, which is what the `list.rs` child did.

### 5b. The original observation

In the `list.rs` child, a patch with two context anchors and no added or removed
lines returned `"Success. Updated the following files:\nM …/src/panels/list.rs"`
with a `FileChange` receipt whose `unified_diff` was empty. `git diff` showed no
modification. The child noticed and reissued; an agent trusting the receipt would
not have.

### 6. Settlement notifications measure the wrong actor's clock

The parent was told *"request 1 'help-test' settled Ready (+5m27s since actor
launch)"*. That child was admitted about seventy seconds earlier. The figure
matches the age of the **root's own session** to within two seconds, for both
children reported. `tidepool/src/actor_host.rs:4396` renders the event with
`observation.snapshot().launched_at_unix_ms` — the launch time of the actor
*receiving* the notice, not the actor it is about.

**Fixed.** The value was right and the label was lying about it. The label now
reads `+12m34s into your session`, the parameter is named `reader_launched_at`,
and a test asserts the text does not claim to measure the actor the event is
about. The useful number — a child's real elapsed time — is separately available:
the host already exposes each actor's `launched_at_unix_ms` as a value precisely
so a parent can subtract it rather than parse prose.

### 7. `doc <topic>` refuses while a skill by that name exists

Hunting for how to run a command, the `evalA` lead asked `doc command`:

> error: unknown Shoal documentation topic `command`; topics: tree (worktree),
> workbench, request, unfold, watch, deadline, refinement, lineage, cleanup,
> recovery, jev, actors; `doc topics` lists the workspace skills; workspace
> modules: …

The workspace ships `shoal-command`, whose entire subject is running commands.
`tidepool-actor/src/prompt_catalog.rs:148` builds that refusal; twenty lines
above, the `topics` branch already holds the skill list with `shoal-command` in
it. The same happened to the `evalB` lead with `doc context`.

### 8. A lookup failure leaks its own scratch file

```
error: /tmp/nix-shell.1bCMmT/.tmpQMOXBc/query-5/Expr.hs:60:33:
  Not in scope: type constructor or class `Command'
```

### 9. Smaller

- A cell whose first line is a `{-# LANGUAGE … #-}` pragma produces a receipt
  reading `defined  at generation 3` — two spaces, no name.
- `RunResult` returns `no match` from a lookup whose own output, in the same
  response, contains `Cmd.stdout :: RunResult -> Either OutputIssue Text`.
- Every type-directed query is padded with `maxBound`, `mempty`, `pi`,
  `undefined` and two unavailable `Jev.Core` names, whatever was asked.
- A truncated module listing ends `… more matches omitted` with no count and no
  way to ask for the rest.
- Factoring three near-identical children into one `let` helper fails with
  `Ambiguous type variable 'parent0'`. Inlining works. Nothing the leads read
  warns of this, though the error itself does say to add a signature.

## What the integration seam made easy, and what it did not

The last `evalB` exercise was the one run 7 never reached: take an integration
worktree, merge each candidate with a check after each, then deliberately go red
and roll back. It completed in 4m45s. Its own split, unprompted:

**The seat made easy** — allocating an isolated integration worktree from a
named ref; merging exact candidate OIDs through `tryMerge`; distinguishing
`FastForwarded` from `AlreadyContained`; preserving typed worktree and merge
receipts.

**Hand-rolled with shell git** — looking up branch heads; running the check
after every candidate; creating and committing the deliberately failing
candidate; its explicit `--no-ff` merge; rolling that merge back with
`git reset --hard HEAD^`; the final green verification.

So the typed surface covers allocating and merging, and everything that makes a
merge *safe* — check after merge, roll back on red, verify green — is shell the
model writes itself each time. That is the concrete shape of the missing
integrator primitive, and it is worth more than the eight bugs below.

Note also that the lead found bug 1 on its own, from the merge results:
*"Detecting from the merge results that the help and list workers had left their
edits uncommitted, so those two branches carried no candidate change."* The
information was recoverable — four exercises and one deliberate merge later.

### Checked independently, not taken on the lead's word

| claim | evidence |
|---|---|
| the rollback restored the exact prior OID | the integration worktree is at `50e4781`, clean, which is precisely the state before the red merge |
| the red work was unmerged, not destroyed | `a770e97 test: deliberately make integration red` still exists on its own branch, parented on `50e4781` |
| the green candidate advanced the intended integration ref | **no.** `shoal/integration` is still at `65bbb44`. Everything happened on the worktree's own branch and nothing was published to the base. The scenario stopped one step short of the thing that would make it real |
| cleanup receipts complete for every role | complete for the three leaves. There was no supervisor or integrator actor in this run — the lead hand-rolled integration — so there is no third receipt to check, and the two leaf worktrees named in bug 2 are still on disk |

## Where the time went

Roughly a third of the `evalA` session was spent waiting on tools, and discovery
dominated it: seven lookups totalling 112 seconds, the worst single round 37.3
seconds, mostly returning `no match`. Finding out how to run a command took four
rounds and about two minutes. In `evalB`, 108 of the 259 seconds of the
admission turn were lookups, driven by the `Selected`/`selected` mismatch above.

The children spent none of their time on discovery. They read source with
ordinary shell commands and their overhead was compile time.

`doc topics` answers in 2 ms. A six-name browse takes 27 seconds. A model that
batches its queries — the right instinct — pays for all of them at once and sees
nothing until they all finish.

## Second pass: what four parallel readers found in the transcripts

The nine findings above came from watching the runs live. Afterwards four
readers went through the rollouts, the durable inboxes, the host logs and the
compiler logs, each on one axis: discovery, cell authoring, delegation and
lifecycle, and Jev. What follows is what they found that watching did not, with
the duplicates against the nine dropped.

### 10. The ready-made merge actor was found and then not used

The `evalB` lead looked up `Project.Merge`, and the same response showed it
`RedRolledBack GitOid GitOid CheckResult` — the rollback outcome it was about to
spend nine model turns reimplementing. It then wrote twelve tool calls of raw
`tryMerge`, `cargo test` and `git reset --hard HEAD^`, taking 151 seconds. The
`evalA` lead used the actor for the same shape of job and it took four calls.

The cost is not only the turns. It means the rollback scenario exercised a
hand-written substitute and proved nothing about the mechanism we wanted tested.

`shoal-orchestrate` already says that a transition determined by a child's
outcome belongs in Haskell rather than a model turn. What it does not say is to
look in the workspace-modules list for an actor that already does it before
writing raw git. That is a one-line addition and it is the highest-value change
on this list.

**Why it chose the primitive is not recoverable.** Codex encrypts its reasoning
payloads, so all four readers hit the same wall. If we want motive we have to ask
a live lead; a rollout cannot answer it.

### 11. No child was ever told to commit

This is the cause of finding 1. The lead built all three assignments from one
shared string asking for a test and a summary. `grep` across all three durable
inboxes finds no occurrence of "commit". One child inferred it; two saw
` M src/panels/help.rs` in their own `git status` and replied as done anyway.

`shoal-unfold` tells the **parent** that "a typed reply is evidence of execution,
not of integration; verify the submitted commit before merging". It never tells
the **child** to make one. The fix belongs in the child's task template.

### 12. Discovery misreports names that are real and advertised

Two directions, one underlying problem.

`renderGitOid` returns `no match`. It is defined at
`haskell/lib/Tidepool/Worktree.hs:398` and **exported at line 131**, and two
shipped skills use it in their worked examples. Needing a `GitOid`, the `evalA`
lead queried six plausible names, got nothing, gave up on discovery, and pasted a
hex string it had read from a `git log` eight minutes earlier in an unrelated
exercise.

`R.start` and `R.client` come back tagged `[unknown]` although
`shoal-define-actors` lists both under "Shipped — in every Shoal cell", and both
worked on first use. The cause is in the source: availability is decided by
whether a callable's *required effects* fit the asking actor's row, and these
carry `Derive`, `Generic` and `GActor` constraints, which are not `Member X effs`
shapes, so classification falls through to unknown. The lead only used them
because it had read the skill first; one that trusted the tag would have gone
looking for an alternative that does not exist.

### 13. Half the compiler daemon's time is not compiling

Across `evalA`, 112 compiler requests totalled 305 seconds of elapsed daemon
time, while the compile summaries in the same log account for 148 seconds of
work. Forty-six individual requests each exceeded two seconds.

The per-cell trace is sharper. The cell that first started `Project.Merge` took
33.4 seconds and issued **nine** separate compiler requests, six of them costing
2.2 to 3.9 seconds, while each request's own reported work was 40 to 260
milliseconds. The log shows why: `tidepool-memo-miss module=Tidepool.Session.Lib.G2…G6
reason=dependency-miss`, one round trip per newly needed module. A cell touching
only warm modules finishes in under a second.

If that batches into one request per cell, it is the largest single latency win
available — larger than anything in discovery.

### 14. Smaller, still worth fixing

- **`Project.Merge` calls a check script a review.** `ReviewRun` and "replied to
  the review" label a `check.sh` exit code. A reader of
  `Published … check.sh=ok(review-run)` could reasonably believe a change was
  reviewed. Naming, not behaviour.
- **`doc tree` is a 4,900-character essay** whose load-bearing sentence about
  `tryMerge` sits about 4,600 characters in.
- **The documented way to stop a session produces a terminal error.**
  `shoal init` prints `stop: tmux kill-session -t <session>`. Doing exactly that
  left both hosts logging `Shoal actor host failed … Resources still retained …
  Connection refused (os error 111)`, listing every resource class as
  unconfirmed. Recorded here because a reader of those logs would reasonably
  conclude something broke; what actually happened is that the advertised stop
  command removes the process supervisor the shutdown path then tries to reach.
- **The children never called `lookup` once.** All three went straight to shell
  reads, `apply_patch`, `cargo test` and `respond`. Discovery friction is a
  lead-only phenomenon, confined to orchestration vocabulary.

### Fixed so far

Finding 3, `updateRequest`. It now refuses with `ReplyError::AlreadySettled` at
submit time rather than accepting an update it has already classified as
undeliverable, and the delivery is no longer optional, so a success necessarily
carries one. The genuine race is untouched: an update deliverable when sent that
loses the window before presentation still reports `UpdateTooLate` on poll.

Two tests. A unit test reconstructing the live situation — a child that has
replied, a correction naming it — asserting the refusal and that no sequence
number is spent. And the live Haskell-surface test at
`tidepool/src/actor_host.rs`, which previously asserted a successful send
followed by `Right UpdateTooLate` and now asserts `Left ReplyAlreadySettled`.

## One thing that is not a bug but should be known

Between exercises the `evalA` lead was told its previous cell had printed 7,247
bytes of a file it read. On the next exercise it reused the binding it already
had and said so. Feeding an observation back changed the behaviour immediately;
the same observation written in a skill had not.

Separately, the `evalB` lead silently rewrote the operator's request into
stricter instructions for its children — adding "Modify no other file", "Do not
delegate", and a required reply shape that the operator never asked for. The
children received those as if they were the original ask. Worth knowing when
reading a child's transcript as evidence of what was requested.

## Astra run, 2026-09-17: the observation budget rejects committed work

The test pilot hit `observation budget 100000 exhausted` twice, both on binds.
One had already committed 45 operations — 10 Jev calls, 35 command jobs — and
lost all of them. The other was `reflect 3`, a single operation, which cannot
bind at all: the budget charges one unit per value node *and per payload byte*,
so it is a ~100 KB ceiling, and three turns of a working session exceed it.

The limit applies at display materialization, not at computing and not at
retention — `run_entry_retained` issues the binding's handle without consulting
any budget — but its failure was promoted to a unit-level rejection that
discarded the handle. Full trace, both cells verbatim, and both tool outputs:
`plans/jev-lab/observation-limit/`.
