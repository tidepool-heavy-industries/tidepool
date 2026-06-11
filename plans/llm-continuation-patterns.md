# LLM-in-Tidepool: Continuation Patterns

Design catalog, 2026-06-11. Focus: how the two intelligence channels — `llm`
(inline, server-side model call, no suspension) and `ask` (suspend; the
CALLING agent answers and resumes) — compose with the continuation machinery.
Jobs and durability are deferred; every pattern here works today, sequentially,
and every one becomes a durable workflow unchanged when effect-log resume
lands (the continuation discipline IS the workflow discipline).

The one-sentence model: **a tidepool program is a coroutine; `ask` is its
yield; the calling agent is its scheduler; the continuation — not KV — is its
call stack.**

---

## P1. The Aperture (canonical, refined)

Compute cheaply → suspend with a *digest* → the caller scouts freely during
the gap → resume with steering → the expensive tail runs informed.

```haskell
hits <- sgFind Rust "struct $NAME { $$$F }" ["tidepool-codegen/src/"]
answer <- ask ("Found " <> pack (show (length hits)) <> " structs.\n\
               \1. classify all  2. emit/ only  3. abort")
case answer of
  String "3" -> pure (toJSON "aborted")
  String "2" -> classify (filter (isInfixOf "emit" . mFile) hits)
  _          -> classify hits
```

The gap is a **free intelligence window**: a suspended continuation costs
nothing server-side, and the caller can run any other tools/evals before
resuming. Sub-forms: *menu* aperture (`Asks.choose`), *open* aperture
(free-text question), *gate* aperture ("about to write 14 files — proceed?").
Discipline: **acts go after apertures; pure scouting never needs one.**

## P2. The Interview (progressive narrowing)

A chain of asks, each narrowing scope — branch → file → function — where the
refined state rides **in the continuation**, not in KV. Conversation as
control flow.

```haskell
files <- glob "tidepool-*/src/**/*.rs"
f  <- Asks.choose "Which file?" (stake 9 (map fst (Explore.sizeRank 9 pairs)))
fns <- defWithContext (files !! f) ...
sel <- Asks.choose "Which function?" (map fnName fns)
deepDive (fns !! sel)
```

Each resume is a typed re-entry: the program cannot lose the thread, the
caller cannot resume into the wrong state. Contrast with the anti-pattern of
one eval per step + KV handoff (loses types, leaks names, re-scans).

## P3. The Escalator (three-tier judgment cascade)

Judgment as a fallback chain: pure heuristic → inline model → suspend to the
caller. `Flow.escalate` is the seed; the general shape:

```haskell
verdict <- case cheapRule item of            -- tier 1: pure, free
  Just v  -> pure v
  Nothing -> do
    r <- yn ?! ("Is this a flaky test? " <> render item)   -- tier 2: inline
    case r of
      Sure v     -> pure v
      Unsure _ _ -> do                        -- tier 3: suspend to caller
        a <- ask ("Can't tell — flaky? y/n\n" <> render item)
        pure (a == String "y")
```

Each tier boundary is a natural continuation point. The `?!` confidence split
(`Sure`/`Unsure`) is the routing primitive between tiers 2 and 3.

## P4. The Tribunal (survey, then adjudicate the contested)

Batch inline judgment over N items; suspend ONCE with only the disagreements
or low-confidence cases. The continuation holds the full classification —
the caller adjudicates a handful, not the corpus.

```haskell
verdicts <- mapM (\x -> (,) x <$> (pick cats ?! render x)) items
let (sure, unsure) = partition (isSure . snd) verdicts
ruling <- ask ("Classified " <> pack (show (length sure)) <> " confidently.\n\
              \Contested:\n" <> bulleted (map (render . fst) unsure) <>
              "\nReply with category per line, or 'skip'.")
finalize sure (parseRuling ruling unsure)
```

This is the right pattern for the "classify all the structs" archetype at
scale: the caller's attention is spent only where the model's confidence ran
out. (`triage`/`survey` are pre-built degenerate forms.)

## P5. The Deferred Decision (compute alternatives, then ask)

Invert "ask permission first": compute every branch's cheap prefix, suspend
with the alternatives **in hand**, let the caller pick with evidence.

```haskell
candidates <- mapM (\style -> (,) style <$> draftRewrite style fn) styles
choice <- Asks.choose "Three rewrites drafted — apply which?"
                      (map (preview . snd) candidates)
patchFile path (fnText fn) (snd (candidates !! choice))
```

Possible precisely because suspension is cheap and the continuation holds all
branch state. The dual of P1: aperture steers *before* the work; deferred
decision steers *after* the cheap work, before the commitment.

## P6. The Socratic Checkpoint (ask as reviewer)

The program computes a result AND a self-check digest, then suspends for
validation before persisting. Differs from P1 in direction: aperture steers
forward, checkpoint validates backward.

```haskell
report <- buildFindingsTable findings
ok <- Asks.approve ("About to persist. Sanity-check the totals:\n"
                    <> summarize report)
if ok then kvSet "campaign_report" report >> pure "saved"
      else pure "held — tell me what's wrong and re-run"
```

## P7. Continuation vs KV (the state discipline)

Two stores, two jobs:

| | continuation | KV |
|---|---|---|
| scope | one logical task, across asks | across evals/sessions |
| typing | full Haskell types | `Value` |
| cost | free, automatic | manual naming, manual parsing |

Rule: thread state through the continuation while a single logical task is in
flight; touch KV only at task **boundaries** (P6's final persist, P2's
entry). Anti-pattern: kvSet/kvGet ping-pong inside one flow — it forfeits
types to save a suspension that costs nothing.

## P8. Stub-and-Pull (the pagination protocol, generalized)

Already shipped for big results: return a digest + stub handles, suspend;
resume fetches chunks or ends. Treat it as a *general idiom* for any verb
with unbounded output: `view` on a large file should be a suspended pager;
a census over a big tree should stub its long tail. The caller pulls
exactly what it needs; the continuation holds the rest.

## P9. The Ask Contract (re-entry discipline)

What separates a good suspension from a bad one:

1. **Shape the reply**: state the expected response form in the prompt
   ("reply 1-3", "reply with a file path", "y/n") — `Q`'s schema machinery
   does this for `llm`; `ask` prompts must do it by convention.
2. **Digest, don't dump**: include enough context that the caller needn't
   re-scout, little enough that it reads in one glance. Counts + exemplars,
   not corpora.
3. **Defensive resume**: every `case answer of` ends in a safe default arm.
   The caller is an LLM; replies are *probably* well-shaped.
4. **One decision per suspension**: a mega-ask bundling three questions
   forces the caller to answer all three before any tail runs — split them
   (P2) or menu them.

## Anti-patterns

- **ask-as-print** — suspending to show progress. Use `putStrLn`/`say`;
  suspension is for *decisions*.
- **mega-ask** — see P9.4.
- **KV ping-pong** — see P7.
- **premature ask** — asking a question the next 10ms of computation would
  answer. Compute first (P5).
- **shapeless ask** — free-form question, free-form answer, brittle parse.

---

## The durability note (why these patterns are the workflow story)

Every multi-ask program above is already a dynamic workflow: bounded fan-out,
deterministic script around nondeterministic calls, suspension points held
server-side. When effect-log resume lands (future-plans A), these exact
programs gain kill-and-resume / fork-at-the-ask for free — the log replays
to the last suspension and continues. **Nothing in the pattern layer
changes.** That is why patterns-first is the right order: we are writing the
workflow library before the workflow engine, against an interface (the
continuation) that the engine already honors.
