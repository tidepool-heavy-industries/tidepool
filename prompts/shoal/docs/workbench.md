Send raw Haskell to the tool. Outside `:{` / `:}`, each nonblank line is one
input unit. Use the delimiters for a multiline declaration group or binding.
Inside a declaration group, use `name = value` alongside `data` and function
definitions; do not mix in GHCi `let` statements. Use a separate unit for those.
These calls need no repository setup:

```haskell
:{
data Candidate = Candidate { candidateScore :: Int }
score candidate = candidateScore candidate
:}
let candidate = Candidate 7
let judge = \value -> score value >= 5
:type judge
judge candidate
[fmt|score={score candidate:d}|]
```

An opaque function is a useful value, not a rendering failure. Ask for its
type or apply a pure projection. `:info fmt` and `:type [fmt|hello|]` use the
same quasiquoter imports as execution. Use `:bindings` to inspect current
names and `:browse` for library declarations.

When retained evidence becomes noisy, keep the original and define a local view:

```haskell
let scores = map candidateScore
let candidates = [Candidate 7, Candidate 3]
scores candidates
scores (filter judge candidates)
```

The view is ordinary Haskell, so you can change it as the question changes.
Long single-line values receive generic indentation; custom rendering and all
underlying evidence remain available. Project before printing when you need
fewer facts, rather than repeatedly dumping the complete value.

Declarations and bindings persist between calls. Earlier closures keep the
definitions they captured; later definitions do not rewrite old values or
children. A retained recipient needs the new decision delta when you change
your vocabulary. Do not interpret same-spelled type names from distinct
scopes or declaration generations as interchangeable.

A rejected effectful input unit stops the remaining executable suffix.
Previously completed effects are not rolled back, and the failed unit's
projected bindings are not installed. Inspect its receipt before retrying:
exact transport retries retain the original result, while submitting the
same source in a new call is new intent. `:recovery` describes the supported
source-replay boundary; it does not restore arbitrary lost live values.

For native commands, use the `workspace_path` shown in `:status`. Hosted actors
see their assigned checkout at one stable path; different actors can therefore
report the same `pwd`. `host_storage_path` (and worktree receipt `cwd`) names
the backing checkout on the host, not the preferred native-tool working directory.
Status also shows the assigned worktree and expected branch when available.
Check `git branch --show-current` and Git worktree identity against that assignment
before diagnosing a routing problem. The expected branch is launch evidence;
an intentional checkout change can make current Git state differ.

The concise status roster shows each actor's actual `supervisor` identity.
Actor labels are names, not proof of ancestry; `:lineage` exposes supervision
and context ancestry separately. Request settlement and runtime posture do not
establish that a parent has reviewed, accepted, or integrated a submission.
