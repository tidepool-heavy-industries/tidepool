# 09 — Hygiene: scripts, docs, LSP daemon, dead code

The no-scar-tissue sweep: stale shadows, dead references, doc-vs-reality
drift. Mostly deletions and one-line corrections; two real (low-sev) bugs in
the LSP daemon.

## ANTI-PATTERNS

- Per the no-scar-tissue rule: a doc-vs-reality gap gets the DOC fixed or the
  CODE fixed — never a warning note layered on top.
- Don't fold doc fixes that belong with code changes into this tier — each
  code-owning plan (01–08) carries its own doc-drift list; this file is the
  residue that stands alone.

---

## F1 (HIGH): `scripts/deploy.sh` is a stale shadow of `redeploy.sh` — silently deploys nothing for the extract

**Where:** `scripts/deploy.sh:18`. Step 2 copies the cabal build to
`~/.local/bin/tidepool-extract-bin` — but the live resolution path is the
nix-profile wrapper (`~/.nix-profile/bin/tidepool-extract`, earlier on PATH);
`haskell/CLAUDE.md` explicitly documents that a `cp` there "does nothing …
stale cruft shadowed by the nix-profile entry." Wrong basename too
(`tidepool-extract-bin` vs `tidepool-extract`), and it never installs
`tidepool-repl`.

**Failure:** change `haskell/Translate.hs`, run `deploy.sh`, see "Deployed.",
reconnect — the live server still runs the old extract. Exactly the
silent-stale-extract class the merge/deploy protocol was hardened against.
Nothing references `deploy.sh` in any doc; both CLAUDE.md files point only at
`redeploy.sh`.

**Fix:** DELETE `scripts/deploy.sh`. If a nix-free fast path is wanted, fold a
`--cabal-extract` flag into `redeploy.sh` that warns it does not update the
nix-profile binary.

## F2 (HIGH → resolved by this review): root CLAUDE.md ordered every session to read a deleted file

Root CLAUDE.md (Rules → Plans) says "`plans/README.md` tracks the current
active plan. Read it before starting new work." — `plans/` was removed in
`cc90fe07`. **This review recreated `plans/README.md`**, so the reference is
live again. Residual action: none, unless plans/ is retired again — then also
drop the rule.

## F3 (LOW): LSP daemon unconditionally unlinks a possibly-live socket; loser daemon leaks

**Where:** `tidepool-lsp/src/main.rs:72` — `let _ =
std::fs::remove_file(&sock_path);` before bind, no liveness probe. A second
daemon on the same root silently steals the path; the first keeps running
with its rust-analyzer (indexing CPU + multi-GB RSS), unreachable forever —
an orphan only discoverable via `ps`.
**Fix:** try `UnixStream::connect` first; if it succeeds, exit with "daemon
already serving <path>"; unlink only on connection-refused (stale socket).

## F4 (LOW): tidepool-lsp/CLAUDE.md claims requests are "never blocked waiting on indexing"; the daemon rejects them until indexed

**Where:** `tidepool-lsp/CLAUDE.md:25-28` vs `tidepool-lsp/src/main.rs:129-135`.
`dispatch` returns "rust-analyzer not ready" for every op except `status`
until a `$/progress` END event flips `ready` (`jsonrpc.rs:356-360`); the 600s
cap only changes the LOG LINE, never the gate. If RA never emits a matching
end token (token renames across RA versions), all LSP verbs error forever
while the doc promises degraded service.
**Fix:** either flip `ready` after the 600s cap (making the doc true —
recommended) or correct the doc.

## F5: misc drift / dead files

- Root CLAUDE.md Project Structure omits `tidepool-bridge-effects/` — a real
  workspace member (`eval_harness.rs` imports it). Add the row.
- `examples/tide/Cargo.lock` is dead: `examples/tide` is a workspace member,
  so cargo ignores the nested lockfile — it only misleads. Delete.
- Clippy sweep: ~25 style-tier warnings workspace-wide (`cargo clippy --fix`
  candidates; list in plan 07 opportunities).

## Doc-drift lists carried by other plans (index)

So a doc-focused session can sweep everything: plan 01 (codegen CLAUDE.md
PrimOpKind claim + SeqOp gap, gc.rs C6 rationale inversion, emit/mod scaffold
comments, jit_machine tenure/Abort comments, errors.rs garbled docs, heap
layout.rs/arena.rs narration), plan 02 (CborEncode arity comment), plan 03
(cache.rs fingerprint doc, machine.rs deep-force comment), plan 05
(datacon_table get_by_name comment, deep_force MAX_DEPTH comment), plan 06
(repl CLAUDE.md: it-binding/HUGE_CEILING/bind-turn cost + value-null claim),
plan 07 (mcp CLAUDE.md structural-search section, tidepool lib.rs handler
list).

## Verified clean — do NOT re-audit

`redeploy.sh` itself; `tidepool-lsp` `resolve.rs` (path-containment sandbox,
UTF-16 handling, rename-probe union) and `diff.rs`; `.config/nextest.toml`
hazard audit matches code (no test binds the daemon socket); root CLAUDE.md
Eval Records API table matches `haskell/lib/Tidepool/Records.hs` and the
generated effects surface.

## DONE CRITERIA

- [x] `scripts/deploy.sh` deleted (or absorbed as a warned `redeploy.sh` flag)
- [x] F3 liveness probe; F4 gate-or-doc resolved
- [x] F5 rows added/files deleted (clippy sweep tracked in plan 07, not this tier)
- [ ] Doc-drift index above fully swept (may land inside plans 01–08's PRs)
