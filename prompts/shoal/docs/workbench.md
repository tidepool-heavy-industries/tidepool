Send raw Haskell to the tool. Outside `:{` / `:}`, each nonblank line is one
input unit. Use the delimiters for a multiline declaration group or binding.
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
