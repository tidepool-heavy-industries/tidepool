# lab7 — Jev linting our own tooling

Theme: point cheap semantic judgment at text we wrote for models to read,
deliberately outside the agent-swarm workflow. Three experiments, one noul
pool each.

Friction note: launch against the shared `~/dev/shoal-evals/tui-test-app`
workspace failed for lab6/lab7/lab8 with an `.owner.lock` collision (single
Shoal session per workspace, keyed on the workspace path, not the session
name) — logged as friction item 11. The coordinator supplied a private clone
at `/home/inanna/.claude/jobs/4940a626/tmp/ws7`; all three cells below ran
there.

---

## 50. Error message lint

1. **Documented**: a noul-per-item pool, the same calibrated yes/no rubric
   applied uniformly across every pool member (TypeSafe's basic
   ask-the-same-question-of-every-item pattern).
2. **Ours**: pointing that rubric at the engine's own `bail!`/`anyhow!`/
   `#[error(...)]` refusal-message templates, with one hand-picked "gold"
   message (the mount-boundary refusal) planted inside the same pool as a
   canary rather than checked separately.
3. **Fixture**: 25 real message templates gathered with ripgrep from
   `tidepool-actor/src`, `tidepool-runtime/src`, and `tidepool-node/src`
   (`bail!`, `anyhow!`, `#[error("...")]`), plus the mount-boundary canary
   text pasted verbatim, saved to
   `/home/inanna/.claude/jobs/4940a626/tmp/lab7/exp50-messages.txt`.
4. **Questions** (asked per pool item, verbatim):
   - "Does this message name the specific thing that was refused or that
     failed, rather than only the category of failure?"
   - "Does this message name a concrete action, directory, value or
     alternative that would have worked instead?"
   - "Does this message name the state or condition that caused the
     failure, such as what the actor holds or where it is?"
5. **Numbers** (raw, truncated to 60 chars; columns = specific / actionable /
   cause):

   | message | specific | actionable | cause |
   |---|---|---|---|
   | (canary) this actor cannot run a command in <dir>: worki | 0.79 | 0.81 | 0.96 |
   | actor {actor} rejected the invocation: {detail} | 0.28 | 0.11 | 0.26 |
   | agent {agent:?} exceeded the runtime tool-round backstop | 0.57 | 0.14 | 0.82 |
   | busy: every pool slot is occupied and none could be evic | 0.32 | 0.05 | 0.88 |
   | cannot acknowledge sequence {requested}; current cursor  | 0.83 | 0.24 | 0.88 |
   | declaration type-check failed: {0} | 0.30 | 0.06 | 0.32 |
   | dirty submodule is unsupported in v1: {} | 0.85 | 0.21 | 0.85 |
   | facade target has no parent directory: {} | 0.64 | 0.09 | 0.90 |
   | input digest must be exactly 64 lowercase hexadecimal ch | 0.42 | 0.56 | 0.73 |
   | interactive input contains {actual} bytes; limit is {li | 0.59 | 0.16 | 0.79 |
   | managed worktree {0} is registered but missing on disk | 0.73 | 0.21 | 0.91 |
   | mount target must be absolute and contain no parent tra | 0.37 | 0.35 | 0.69 |
   | no managed worktree registered with id {0} | 0.59 | 0.16 | 0.86 |
   | overlay target {} is outside the model-visible project  | 0.65 | 0.25 | 0.92 |
   | process supervisor path must be absolute: {0} | 0.42 | 0.31 | 0.72 |
   | registry root {} resolves inside the git working tree a | 0.73 | 0.72 | 0.94 |
   | request {method} timed out after {timeout:?} | 0.67 | 0.11 | 0.71 |
   | resident tool dispatch panicked before returning its fu | 0.55 | 0.05 | 0.67 |
   | scope claim requires a live actor | 0.46 | 0.14 | 0.77 |
   | select actor effects in its profile or launch options,  | 0.45 | 0.73 | 0.50 |
   | session {0} has no parked hole; a child run requires a  | 0.73 | 0.45 | 0.90 |
   | source repository is dirty: {0} | 0.51 | 0.11 | 0.93 |
   | the executing principal is not authorized for worktree  | 0.84 | 0.18 | 0.67 |
   | tool `{tool}` expected {expected} arguments, received { | 0.82 | 0.38 | 0.73 |
   | worktree {worktree} is already bound to agent {holder} | 0.83 | 0.26 | 0.93 |
   | writable root {} is not inside a protected read-only ro | 0.58 | 0.29 | 0.89 |

   Canary check: specific 0.79, actionable 0.81, cause 0.96 — all clearly
   above 0.5, on the high end of the whole pool. Canary held; the run is
   live, not void.

   Messages below 0.5 on **all three** axes (worth rewriting):
   - `actor {actor} rejected the invocation: {detail}` (0.28 / 0.11 / 0.26)
   - `declaration type-check failed: {0}` (0.30 / 0.06 / 0.32)

   Observed: almost every message scores high on "cause" (most engine
   messages already say *what state* is wrong) but low on "actionable" —
   very few name a working alternative the way the canary does. The two
   below-threshold messages are also the two that are pure passthroughs of
   an opaque inner error (`{detail}`, `{0}`) with no engine-authored content
   of their own.
6. **Outcome**: **worked**. Canary validated the packet, and the pool
   surfaced two real, specific messages to rewrite rather than a vague
   "most error messages are bad" impression.
7. This would absorb a human's pass of reading every `bail!`/`#[error]` site
   in a crate by hand to flag which refusals fail the "names three things"
   bar.

---

## 51. Skill example lint

1. **Documented**: same noul-per-item pool pattern, applied to source code
   rather than prose.
2. **Ours**: linting a skill's own fenced Haskell examples for stale-name
   drift against a shipped-names list carried in `J.state`, instead of a
   human eyeballing each code fence against the skill's own prose claims.
3. **Fixture**: 11 real fenced Haskell blocks, extracted **whole and
   unedited** from `.shoal/skills/*/SKILL.md` (`shoal-cleanup`,
   `shoal-command` x3, `shoal-jev` x2, `shoal-coordinate`, `shoal-review`,
   `shoal-fork`, `shoal-unfold`, `shoal-define-actors`) — corrected mid-run
   after the coordinator flagged that an earlier draft had paraphrased/
   truncated some blocks rather than pooling the real code; a first re-run
   on the full 16-block set also hit the cell's shared display budget
   (partial `cellDisplay.more` truncation on the returned JSON), so the
   final pool was cut to 11 blocks, still whole and unedited, to fit.
   `shipped_names` = the list from `shoal-jev/SKILL.md` lines 13-20 plus
   `Cmd.run`, `Cmd.argv`, `Cmd.stdout`, `R.start`, `R.client`, `R.call`,
   `unfold`, `respond`, per the brief.
4. **Questions** (per pool item, verbatim):
   - "Does this example use only names that appear in `shipped_names`?"
   - "Does this example refer to a type or function by a name that
     `shipped_names` does not contain?"
5. **Numbers** (raw):

   | block | only_shipped | has_unshipped | actually contains an unshipped call? |
   |---|---|---|---|
   | cleanup0 | 0.07 | 0.93 | yes (`planCleanupFor`) |
   | command0 | 0.16 | 0.78 | **no** — only `Cmd.run`/`Cmd.stdout` |
   | command2 | 0.13 | 0.76 | yes (`Cmd.withArguments`, `Cmd.describe`) |
   | command3 | 0.15 | 0.68 | yes (`Cmd.output`/`job`/`pageText`/`next`) |
   | jev0 | 0.13 | 0.85 | yes (`Cmd.withArguments`) |
   | jev1 | 0.21 | 0.74 | **no** — only `J.pool`/`noul`/`eachIn`/`askAbout`/`ask`/`state`/`answers` |
   | coordinate1 | 0.06 | 0.93 | yes (`followWork`/`notifyWork`/`readWork`/`inspectFull`) |
   | review1 | 0.09 | 0.91 | ambiguous — `respond` is shipped; rest are local record fields |
   | fork0 | 0.07 | 0.95 | yes (`withEffort`/`withContext`/`solTaskFrom`/`childWithProgress`) |
   | unfold0 | 0.06 | 0.83 | yes (`batch`/`child`/`coding`) |
   | actors3 | 0.16 | 0.92 | yes (`R.send` is not in `shipped_names`) |

6. **Outcome**: **interesting failure**. `has_unshipped` never drops below
   0.68 even for the two blocks (`command0`, `jev1`) that call *only*
   `shipped_names` verbs, and `only_shipped` never rises above 0.21 anywhere
   in the pool. The two axes never separate the genuinely-stale blocks from
   the genuinely-clean ones — there is no threshold that would work. My
   read (not measured): the question asks about "names" in the example
   without distinguishing a qualified library call (`Cmd.run`) from an
   ordinary local binding (`result`, `changed`, `previews`, `packet`); every
   real snippet has several of the latter, so the noul finds "a name not in
   `shipped_names`" everywhere, true or false. The lint as written cannot do
   the one thing it was built for.
7. This was meant to absorb the "grep each skill's fenced example against
   its own shipped-names list" check that caught a real stale-name defect
   by hand once; as measured, it does not yet do that job — the question
   needs to be scoped to qualified calls specifically, not "any name."

---

## 52. Jev predicts which of our own questions will fail

1. **Documented**: a meta-noul pool — asking Jev to judge *questions* rather
   than artifacts, using the same per-item rubric.
2. **Ours**: feeding Jev's own measured wording A/B (vague vs. narrow
   rewrites of the same repair questions, from `wording-ab.md`) back into a
   pool, blind to which version is which, to see whether a packet can
   flag its own weak wording before it is sent. Two more live examples came
   from the coordinator's own runs (one flat 0.52-0.54 no-signal question,
   one flat 0.04-0.08 question that was "plainly true of the state" but
   still failed) and were folded into the same blind pool as a fourth and
   fifth data point beyond the original four wording-ab.md pairs.
3. **Fixture**: 11 question texts — 7 measured weak/no-signal (0.41, 0.42,
   0.42, 0.18, 0.05, plus the two coordinator live cases) and 4 measured
   strong/separating (0.67/0.28, 0.72/0.12, 0.96/0.89, 0.98/0.02) — pooled
   without labels, from `wording-ab.md` and the coordinator's message.
4. **Questions** (per pool item, verbatim):
   - "Does this question name a field of the state it will be asked
     against, in backticks?"
   - "Does this question state the specific fact that decides the answer,
     rather than asking for an overall judgment?"
   - "Does this question say why the condition would hold, giving something
     concrete to check?"
5. **Numbers** (raw; columns = names_field / states_deciding_fact /
   gives_why; question truncated to 70 chars):

   | question | measured | version | names_field | states_deciding_fact | gives_why |
   |---|---|---|---|---|---|
   | Is at least one listed site in a file that does not start | 0.96/0.89 | strong | 0.88 | 0.43 | 0.23 |
   | Should the repair edit this group's shared location, rathe | 0.42 | weak | 0.07 | 0.14 | 0.13 |
   | Is the code at this group's shared location already correc | 0.67/0.28 | strong | 0.08 | 0.37 | 0.29 |
   | Does repairing this require changing a type or signature t | 0.18 | weak | 0.69 | 0.26 | 0.19 |
   | Does the compiler's own suggested fix insert a placeholder | 0.98/0.02 | strong | 0.12 | 0.56 | 0.39 |
   | Would a single edit resolve every listed site in this grou | 0.42 | weak | 0.06 | 0.15 | 0.13 |
   | Does each listed site need its own separate edit, because  | 0.72/0.12 | strong | 0.07 | 0.45 | 0.60 |
   | Is this a lint that should be allowed where it fires rather | 0.05 | weak | 0.05 | 0.13 | 0.11 |
   | Is the fix the compiler suggests in its own help text the r | 0.41 | weak | 0.09 | 0.13 | 0.16 |
   | Is `load` the right term to search the tree for, rather tha | 0.52-0.54 flat | weak | 0.49 | 0.19 | 0.18 |
   | Does `observations` already state whether the parameter add | 0.04-0.08 flat | weak | 0.77 | 0.41 | 0.20 |

   Group averages: **states_deciding_fact** — strong 0.4525 vs. weak
   0.2014 (strong questions score ~2.25x higher). **gives_why** — strong
   0.3775 vs. weak 0.157 (~2.4x higher). **names_field** — strong 0.2875
   vs. weak 0.317 — no separation, and if anything inverted, because
   backtick presence in this set doesn't track question quality: two of
   the four strong questions use no backtick field at all, while two weak
   questions (`outside `owned_paths`` and `` `observations` ``) do.
6. **Outcome**: **worked**, on 2 of the 3 meta-questions. The coordinator's
   two live cases land where their measured behavior predicts: `load`
   scores 0.19 on states_deciding_fact (weak, matches its flat 0.52-0.54);
   `observations` scores 0.41 (still below the strong-group average of
   0.4525, matches its flat 0.04-0.08 despite reading as "plainly true").
   `states_deciding_fact` and `gives_why` separate strong from weak
   wording; `names_field` does not and should be dropped from this
   particular lint.
7. This would absorb the "eyeball a drafted Jev question and guess whether
   it's going to return calibrated numbers" judgment call a question author
   makes by feel before spending a real packet call to find out.

---

Session `lab7` killed at the end of this run.
