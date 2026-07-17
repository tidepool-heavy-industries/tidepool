# DRAFT: Security boundaries at the effect level, or: why your agent keeps deleting your home directory

*Status: draft v0 — core argument captured, needs voice pass + examples tightened.*

## The problem is that the action layer is made of strings

Every coding agent on the market acts through the same aperture: it emits shell
strings, and a harness decides whether to run them. All the safety machinery —
allowlists, regex filters, permission prompts, "sandboxed bash" — is an attempt
to *analyze strings* whose semantics are defined by bash. Quoting, word
splitting, parameter expansion, subshells, `$(...)`, aliases, env-var
indirection, `xargs`, symlinks: the permission layer has to out-lawyer a
language that was never designed to be analyzed. It can't, and everyone knows
it can't, which is why the failure mode keeps recurring — the "agent deleted my
home directory" reports (Codex being the famous repeat offender) aren't a
model-alignment problem. A model that is *trying* to do the right thing still
emits `rm -rf "$WORKDIR"` with an unset variable, or a `find -delete` with a
path that resolves somewhere unexpected. The action surface itself is fuzzy.

The industry's answer is per-agent VM isolation: give every agent a disposable
machine and let it yolo inside. That works, sort of, but it's heavy, it's slow,
it doesn't compose (the agent still needs your credentials, your repo, your
dotfiles *inside* the VM to be useful), and it concedes the interesting
question. It doesn't make the agent's actions legible — it just relocates the
blast radius.

## Typed effects are a security boundary you can actually reason about

Tidepool's bet: don't analyze the agent's strings — change what the agent
emits. In tidepool, an agent's action is a *typed program* over an explicit
effect stack (`Eff '[Console, KV, Fs, Http, Exec, Lsp, Llm, Git, Time, Ask]`).
Every side effect the program can ever perform is a typed request — `FsWrite
path contents`, `Run cmd`, `HttpGet url` — that crosses exactly one boundary
into a Rust handler the *operator* wrote and controls.

That inverts the security posture in three ways:

**Capability is presence in the stack.** A program whose stack lacks `Exec`
cannot shell out — not "is prompted before shelling out," cannot. There is no
string clever enough to invoke an effect that isn't in the type. Granting
capabilities per-agent, per-session, or per-task is editing a type-level list,
not maintaining a regex allowlist against an adversarial grammar.

**Policy lives in the handler, in a real language.** The `Fs` handler enforces
the workspace sandbox in Rust, at the one chokepoint every file operation flows
through. "Never write outside the worktree" is fifteen lines of path
canonicalization you write once — not a property you hope holds across every
way bash can spell a path. The recurring home-directory deletion is not a rare
edge case that better prompting fixes; in this architecture it's a *category
error*. The verb surface doesn't contain it.

**Every action is audit-grade data.** Effect requests and responses are typed
values (CBOR on the wire). The complete record of what an agent did is not a
chat transcript you skim — it's a trace you can log, diff, replay, and write
policy against. "Show me every write outside `src/`" is a query, not a
forensic reconstruction.

And the escape hatch is honest: `Exec` still exists, because sometimes you need
bash. But it's *one effect* — you can deny it, allowlist it, or wrap it in an
approval `ask`, and its grant status is visible in the type of every program
that uses it. Bash becomes a capability you extend deliberately instead of the
entire substrate everything runs on.

The claim, stated plainly: **per-effect boundaries get you most of what
per-agent VM isolation buys, without the VMs** — and they get you something
VMs never will: an action layer that is legible before it runs.

## Why this is cheap instead of expensive (the fluency argument)

The obvious objection: you've replaced bash, which every model speaks natively,
with a bespoke API, which none do. The counter is the design rule the whole
surface is built around: *the API is the prompt.* The surface is GHCi —
canonical Haskell, `Data.Text`, `Data.Map`, record-dot syntax — which models
are near-natively fluent in from decades of training data, the same way they're
fluent in bash. One eval replaces N tool calls: the agent writes a ten-line
typed program (grep → fold → join → aggregate) where the bash version is four
round-trips of piped string-mangling, each one a fresh chance to get quoting
wrong. Safer and *fewer tokens* is not the usual trade.

## Secondary pitch, worth its own post: off-context computation

The session heap gives agents typed working memory *outside* the context
window: load a 170k-line corpus (or your email, or ten thousand contracts)
into a resident session once, then interrogate it across turns with cheap
folds, surfacing only aggregates. The model computes over data it never reads
into context — which is a privacy architecture and a cost architecture at the
same time. (Sketch: this is the RAG-inversion story; keep separate from the
security post.)

## Notes / TODO

- Concrete before/after: a real bash disaster (unset-var `rm -rf`) vs the same
  intent as an `M`-program hitting the Fs sandbox.
- Numbers for the fluency claim: eval-vs-bash token counts from the track-2
  optimization-loop rounds.
- Honest-limits section: handler quality is the TCB; `Exec` grants reopen the
  string problem for that session; effect surface completeness vs temptation
  to grant Exec.
- Positioning vs. Codex/Claude-Code sandboxing docs, cite the home-dir
  incident reports.
