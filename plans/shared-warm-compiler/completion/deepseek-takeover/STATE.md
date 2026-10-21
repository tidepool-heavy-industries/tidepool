# Exact takeover checkpoint

Historical snapshot, superseded by
[the current closeout state](../deepseek-g2-closeout/STATE.md).
Do not use its branch, failures or worker assignments as the current frontier.

Observed 2026-10-21 after the morning review. No implementation or deployment was
performed during that review. All admitted morning checks are terminal; no new
Sol work is pending. Recheck actual jobs and refs before taking ownership.

## Source and remote identities

| Role | Checkout / branch | Observed revision |
|---|---|---|
| Reviewed source base | /srv/swarm/checkouts/tidepool-latent-original-stage-join-20261020; feature/latent-original-stage-join-20261020 | 655f049a0fffe8ef0127672d0b9d26e2bb79b18b |
| Warm execution checkout | /srv/swarm/checkouts/tidepool-demand-projection-20261020 | 5c9e8aa9124e55eb523e35962be5a47f614b0935 |
| Source corresponding to warm tree | Same tree as warm execution | c86c396830da26e80ea541f9c94cb8726561f085 |
| Shared warm/source tree | Git tree | 1c658fea45bec96a851ca0a9e87fb04a7cfbf3b8 |
| Local canonical integration | /srv/swarm/checkouts/tidepool-join-final-20261019; integration/compiler-query-warm-final | e6a68e8bbbdf80bdeb5eb24492dadb6a15c5c3d9 |
| GitHub integration, last direct observation | integration/compiler-query-warm-final | fd1862b96d48c68cca78a26f569b6b497d069491 |
| GitHub main, last direct observation | main | 545d9950e5c3c3940f5b6bf84dfc5d16d6b1a89d |

The source base contains local canonical e6a68e8bbb (ancestry checked), with
43 additional commits. Canonical itself was 15 commits ahead of GitHub. Earlier
pushes to **origin** were pushes to a local repository, not GitHub publication.
The source checkout has an explicit **github** remote for the real repository.
These are observations, not permission to force-push or overwrite later work.
The documentation commit containing this file is a later, docs-only checkpoint.

The required integration-status.py --strict inventory was run while preparing
this pack. It reports local_main_missing in this feature checkout (exit 1);
the candidate is not thereby proven integrated into main. Harness pins agree
at 1893773e14b02b3015d99ba63114decfd3223949. Do not create a misleading main ref
merely to make that status command green. Reconcile in the actual integration
checkout and verify remote publication explicitly.

Do not switch the interactive /srv/swarm/checkouts/tidepool tree: it is unrelated
active/dirty work on an older branch. Do not reapply already consolidated parcels.

## Latest executed morning gates

All three ran the warm c86-equivalent tree, **not** the later 655 fixture change.

| Gate | Actual result | Interpretation |
|---|---|---|
| Discovery, expected 1 | Stack overflow; executed libtest count unknown | Failed process, not a counted test pass or proof of the introspection body |
| Notebook, expected 4 | Executed 4, passed 0, failed 4 | Every case stops at initial public read: main:Tidepool.Prelude, ArtifactIdentityMismatch; earlier operator-name conflict cleared |
| World queries, expected 2 | Executed 2, passed 1, failed 1 | Source-domain/invalidation case passes; hot-query diagnostic fixture fails Haskell Text/[Char] typechecking before its answer assertions |

All seven case processes and owned compilers have confirmed cleanup. Hosted
cleanup was not observed; complete diagnostic evidence was not established.
Do not upgrade one cleanup observation into another.

Executable SHA-256:

- Discovery: 6beebd2fb9766961ce7f9572fe4e02e48fba1e9a4c1c6a5f736d33d7d30863e9
- Notebook after warm relink: 1e62e6c8c1306fcd6014509a18da99987691a433ca9f3ec1623e50f991f30473
- Queries: 8c12edadea9632a2ccdc0ffb9cbc9e5f09e7b2d4a20d414fc87b8f3a03d2d2ee

Evidence root:
/srv/swarm/evidence/inanna/latent-original-stage-join-20261020/

- morning-review55-commands.json: exact argv, prelaunch resources and hashes.
- morning-review55-results.json: terminal counts, hashed streams and cleanup.
- morning-review55-active.json: terminal operation state; old session IDs are
  not instructions to relaunch or poll a dead operation.
- plan-b-current.json: broader exact-source evidence index.
- introspection-stack53/deepseek-stack-handoff.md: full stack investigation,
  retained executable, next diagnostic recipe and ownership-cut analysis.

## Strong prior results and their limits

- Full finite history passed 1/1 at 3b5523375b805224549952ae00149cdc309a24b1:
  expressions 42/44, retained callable 45, producer removal, captured-A cutoff,
  continued B and callback 87, with cleanup. This is real progress, not final
  acceptance of subsequent source changes.
- The compiler mixed-World path and PLAN6 actual expression/helper/cancellation
  control passed. Native defining-program lifetime, stage refusal, publication
  properties and several custody controls passed at their recorded revisions.
- Status 1, private-input 2 and uncertain-END 1 passed in the preceding joined
  cohort. Preserve their exact-source limits and rerun affected joined gates.
- Cold at 28e8b3238bbfd9f3aa53cc1a1a85ea4e471bb318 executes reader 41 and verifies
  R→G→H before failing an incorrect public-projection Original-role assertion.
  The later 99, producer removal, cold reopen and 41099 oracle were not reached.
- Genuine FIFO eviction/reacquisition, current default reload, current catalog,
  frozen bundle and fresh Persona acceptance remain unqualified.

## Reviewed findings versus hypotheses

1. Confirmed source hole: completion custody can change payload acquisition
   owners without closing selected rows/availability. A later Request close
   may repair only its own projection. See leaf A.
2. Confirmed source defect: a generic owner index includes PackageCode, while a
   HOME predicate consumes it. The exact captured-A offending module was not
   observed; diagnostic 543d737e63307a1ba59f32c14c588fa218dfcb1e is unjoined and
   unexecuted. See leaf B.
3. Confirmed overflow, incompletely observed suffix: compile55 progresses to a
   third checkout beyond the older measured chain. Do not claim the old fault
   instruction is the current fault. See leaf C.
4. Confirmed fixture mismatch: public facade roles need not contain its hidden
   defining Original. Preserve the intended authority/cold semantics instead
   of merely replacing the expected list with empty. See leaf D.
5. The notebook Prelude refusal may share A's cause, but that causal link is
   **unproven**. Record its exact rejecting predicate before declaring it fixed.

## Parcel and worker disposition

- A/W architecture review, B/L HOME audit, E Haskell review and map audit are
  complete. Their substantive findings are incorporated into this pack.
- Original's morning checks and D's stack handoff are terminal; no extra GDB
  replay, repair or broader suite was launched.
- Latest parent/child recovery fixture correction f5eb17d0d0 is integrated in
  655; formatting/source review passed, joined execution remains pending.
- Map/ledger candidate b6682b3c1f is not integrated; reconcile it rather than
  claiming the current map already describes PLAN6/full finite success.
- Private Persona compatibility copy: /srv/swarm/checkouts/LettaPersona-g2-private-qualification-20261021,
  e67e96c5caf3. Only three reviewed compatibility files differ; authored memory
  is retained. No compile/launch acceptance. Receipt:
  /srv/swarm/evidence/inanna/g2-history-tests-20261020/persona-private-copy-handoff.json.
- Pending-init agent names are not active jobs or pending implementations.

Prelaunch resource observation was about 147 GiB scratch free, 105 GiB available
RAM and 39.3 GiB build.slice usage. This is stale capacity data, not admission.
Use current measurements and the installed launcher contract before new work.
