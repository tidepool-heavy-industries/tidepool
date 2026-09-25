# Wave 6 root interview digest

Source: exomonad-harness repo, `docs/exomonad-friction.md` at ddacd42 (9-question
interview) and 260b694 (adds Q10, background commands); orchestration-friction
commit c427057.

## 1. Ten questions, root's answers

1. **First turn.** First tool call read `NEXT.md` alone; second read `README.md`,
   `docs/correction-plan.md`, `docs/tree.md`, `docs/questions.md`. The first
   plan-advancing action (admitting the preflight lead) was the 7th tool call.
   Root proposes one `NEXT.md` paragraph naming exact head (`d0245b3`), settled
   Q4/Q5, and the first concrete action to shorten this.

2. **Children's first turns.** Root won't invent durations. Cites: cache probe
   checked reference inputs/builder before two live requests; item-2 probe
   inspected CLI/transport visibility before declining inference; trace-design
   leaf read builder and demo call/sleep/wait paths; Compactor leaf discovered
   an unowned dependency need (preventable). Wants a first-call-ready brief:
   exact source commit, owned files, one production consumer, first file/line,
   one focused command + expected test count, acceptance/stop-ping condition,
   manifest/module owner.

3. **Operator notes and reload.** Called `reload_agent_spec` once; it did NOT
   succeed — error: `prepared engine: missing imported value
   Project.Shell.presentSelected`. It published the new source layer while the
   old typed tool record stayed active, so "after the reload" became
   ambiguous. Root relayed the refusal and asked the lead to re-run both
   exact-commit reviews under the updated prompt.

4. **Checkpoints.** Core sent admission + settlement checkpoints with children,
   base, owned paths, first expected reply. Root itself did NOT send one
   explicit protocol-form checkpoint after its own fork cells (preflight/probe,
   trace waves) — has no parent `sendMessage` target, calls this a prompting
   gap, not an engine finding.

5. **Fence and status.** Checked per-child delivery lines repeatedly. Quotes
   one: `inbox=open; last_message=ref2 submitted/not-presented; source=d0245b3;
   next=await-event`. Useful for steering decisions; full roster dumps were
   "mostly noise" once state was unchanged.

6. **Blocked replies.** Both reviewers correctly refused to fabricate
   `Accepted` from mismatched `CommitReview` input (cost: two review cycles +
   pending re-review). Compactor implementer correctly refused to edit
   `Cargo.toml`/`Cargo.lock` outside ownership; root added `schemars` on
   master at `c427057`, checked `cargo check -p harness`. Neither Blocked
   reply is completion evidence.

7. **Most annoying unasked friction.** A source advance has multiple
   partially-independent identities: operator checkout, child source head,
   assignment base, child branch tip, review seed, installed workspace tool
   layer. A clean `git rebase master` can still invalidate a candidate against
   its assignment base; a refused spec reload can still publish half a
   source-layer change. Wants one concise "what revision am I actually using,
   and what would this action publish?" view.

8. **What went well.** Refused to call preparation done: Q4's two-request
   measurement integrated as a finding; item-2 spent no inference (no
   auditable trace). Exact base-to-tip path review kept an unowned settings
   tip out of integration. Credits: one-run inference rule, child ownership
   rule, typed replies, Git merge-parent evidence.

9. **One-page next-run entry.** Wants: exact integrated head; accepted
   operator decisions/holds; one row per obligation (owner, source/candidate
   OID, checks with matched counts, unverified behavior, next action); active
   child/request refs and any inbox fence; owner map and consumer seams;
   permitted live-run commands and trace locations; expected-red test
   owner/closing slice; five non-negotiable rules (no inference automation,
   cumulative ownership diff, exact-commit review, branch merge not file
   copy, no completion claim before final integrated checks). Link full
   PRD/plan; don't paste prior status dumps.

10. See section 2 below.

## 2. Background commands (Q10, from 260b694 only)

Would use `--background` for: a long compile, a focused test, or a one-shot
live/manual trace, while reviewing a disjoint candidate or answering a child.

Completion notice must carry: job handle, exact command, working/source
revision, authoritative exit code or signal, whether process+cleanup are
terminal, whether output is complete, a bounded diagnostic tail or focus
match, and a durable full-output reference (OID/job output handle) with a
no-rerun read path. The handle must be registered for a wake before leaving
it unattended.

Streaming last lines (rejected as sole mechanism, "proposal a") helps
diagnose a blocked wait but still pins the model turn and can flood
attention — doesn't let independent work advance.

Named risk: a lead may merge, rebase, or report success while a check on the
*older* source is still running. Mitigation: tie the job to its source OID
and dependent gate, visibly mark `running` until terminal, refuse to count it
as passing or mutate its checkout concurrently. "A notice is evidence of
command completion, not evidence that the integrated revision was checked."

## 3. Orchestration requests (verbatim where short)

- "one paragraph in NEXT.md" naming exact head, settled decisions, and first
  action (Q1).
- A first-call-ready brief naming "the exact source commit, owned file(s),
  one production consumer, the first file/line to inspect, one focused
  command and expected test count, the precise acceptance and stop/ping
  condition, plus who owns any shared manifest or module line" (Q2).
- "Make source-layer publication and spec reload atomic, or expose a
  one-step rollback/rebuild with the exact stale symbol identity" (Q3 /
  table row).
- Explicit protocol-form checkpoint after root's own fork cells (Q4,
  self-identified gap).
- "One concise 'what revision am I actually using, and what would this
  action publish?' view" (Q7).
- One-page next-run entry template, itemized in Q9 (see above).
- Job handle / completion-notice contract for background commands, itemized
  in Q10 (see above).
- Table-row asks also count as requests: typed message states (submitted/
  presented/quoted-answered/incorporated); typed integration target +
  owned-diff preflight before candidate submission; construct review `Task`
  from `CommitReview` in the review helper/prompt; dependency needs shown in
  admission checklist; opt-in redacted request/job correlation with
  fail-closed trace writes.

## 4. Ranked candidate cards (max 8)

1. **Atomic spec reload** — problem: `reload_agent_spec` can publish new
   source layer while leaving the old typed tool record active, making
   "after reload" ambiguous (Q3, hit this run with a real error). Shape:
   make publish+reload atomic, or expose one-step rollback/rebuild naming
   the exact stale symbol. Level: engine.
2. **Revision-identity view** — problem: a source advance has 6+ partially
   independent identities (checkout, child head, assignment base, branch
   tip, review seed, installed tool layer), so rebases/reloads silently
   desync them (Q7, "most annoying unasked friction"). Shape: one view
   answering "what revision am I using, what would this action publish".
   Level: workspace/engine (spans checkout + tool-layer state).
3. **Background job handle contract** — problem: no way to run long
   compiles/tests/live traces without pinning the turn, and no guarantee a
   completed job matches the still-current source (Q10). Shape: job handle
   + completion notice (exit code, terminal/cleanup flag, output-complete
   flag, diagnostic tail, durable full-output OID, no-rerun read), registered
   for wake, tied to source OID with a gate against stale-source passes.
   Level: engine/workspace.
4. **First-call-ready child brief** — problem: children re-derive settled
   context (owned files, consumer, focused command, acceptance condition)
   from scratch, sometimes missing a preventable blocker (Compactor's
   dependency need) (Q2). Shape: standard brief template naming exact
   commit, owned files, one consumer, first file/line, one command +
   expected test count, stop/ping condition, manifest owner. Level:
   prompt-level.
5. **One-page next-run entry** — problem: no single authoritative resume
   point; risk of re-deriving or losing state (Q1, Q9). Shape: template
   listing integrated head, obligations table (owner/OID/checks/unverified/
   next), active refs/fences, owner map, permitted live-run commands, five
   non-negotiable rules. Level: prompt-level (workflow doc).
6. **Typed message states** — problem: submitted-but-not-presented was
   observed and could be mistaken for readback; root had to catch it
   manually (table row, related to Q5/Q10). Shape: first-class states
   (submitted, presented, quoted/answered, incorporated); notify owner on
   presentation failure or inbox=fenced rather than repeated pending
   snapshots. Level: engine.
7. **Root's own checkpoint protocol** — problem: root sent partial brief
   updates after its own fork cells, omitting parts of the checkpoint
   template it expects of children; no parent `sendMessage` target exists
   (Q4, self-identified as prompting gap, not engine evidence). Shape:
   apply the same checkpoint template to root's own fork actions. Level:
   prompt-level.
8. **Owned-diff / dependency preflight at scaffold time** — problem:
   Compactor leaf discovered mid-task it needed `schemars` but didn't own
   `Cargo.toml`/`Cargo.lock`; correctly blocked but cost a scaffold amendment
   + rebase/reassign (table row, echoed in Q2/Q6). Shape: preflight
   dependency additions and manifest ownership at scaffold time; show
   dependency needs in the admission checklist. Level: workspace-level.

Note: item 5 (revision-identity view) and item 8 (dependency preflight)
overlap with other "Root: still open" table rows not part of the numbered
interview proper (e.g. cumulative owned-path diff gate, AgentSpec effect-row
preflight); those are documented in the friction doc's table but not
re-ranked here since the task scoped this digest to the 10 interview
questions plus the orchestration-friction note.
