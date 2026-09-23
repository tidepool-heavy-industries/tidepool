# Headless vibe session, 2026-09-22

Binary: /nix/store/lhvpbjd3j5zmk9yaija9i4x8lx379y75-exomonad (built 12:13, predates
template consolidation; scaffold had 4 Project modules). Project: scratchpad/vibe.

Timings: new 0.5s; check 17.5s; init 48s; first proxy cell 8.5s (provisions workbench);
warm pure cell 0.8-1.0s; shell command cell 2.1s; rejected cell 0.1s; first Jev cell in a
new session 12.5s; warm Jev packet reusing bindings 2.2s; raw lookup 2.3s.

## Bug (reproducible)
Any J.choice packet fails:
  resident workbench execution failed: prepared engine: typed site 12487394914945005776
  is already installed by program ProgramId(1) with different evidence
Minimal cell (cells/11.hs):
  let nextStep = J.choice "Which next step?" (J.alt #profile "Time it" ["true"] J..| J.alt #nothing "Nothing" ["false"])
  picked <- J.ask (J.state (#task := ("t" :: Text))) (#next := nextStep)
  fmap (\a -> J.handle a.next (#profile id J..| #nothing id)) picked
Fails identically on a fresh proxy workbench (--fresh). J.noul packets work in the same
session. ProgramId(1) predates the proxy: likely the root's hosted tools (Shell/Lookup
presenters use J.score). Retest on a HEAD build before filing.

## Friction
- `lookup "X"` in a cell is Prelude.lookup (returns <function>); the hosted lookup is a
  tool, and from a cell it is lookupRaw (LookupRequest [..] False Nothing 3 []). The
  proxy seat has no tools, so lookup ergonomics there are raw.
- A LANGUAGE pragma line reports "defined  at generation N" with an empty name.
- Results render one list element per line and strings unquoted:
  Right [(fib.py,\n0.96,\n0.17), ...]. Hard to read, ambiguous for Text.
- Binding a command's result still echoes its whole observation (session_id, terminal,
  next, stdout) unless wrapped in Cmd.quiet; noisy in multi-command cells.
- Cmd.stdout rendered as `Right ([0, 1, ...]\n)` with the trailing newline inside.

## Good
- Bindings persist across proxy submissions; declarations and statements mix freely.
- Rejections are fast and GHC-native with caret spans.
- A two-file, four-question Jev packet: correct answers (fib exponential 0.96, notes are
  docs 0.75) in one round trip, all inside one cell.
