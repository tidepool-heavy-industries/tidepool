# Lookup scaffold interface

This is the checked handoff between lookup-owned implementation and
coordinator-owned wire/actor files. It is not a second runtime protocol.

## Public invocation

The hosted function is named `lookup`.

```json
{"queries":["awaitSettled",":: Response result -> Await (Settlement result)"]}
```

The canonical schema accepts a nonempty `queries` array of nonempty strings.
There is no raw-string overload in this release. Input order and duplicate
queries are preserved.

The adapter classifies only a trimmed leading `::`:

```text
NameQuery { source }
TypeQuery { source_without_marker }
```

An empty query or empty type body is an error for that query, not for its
siblings. Text after `::` remains Haskell source and is parsed only by GHC.

## Worker result

The lookup-owned result supplied to the coordinator wire is:

```text
LookupResult {
  query: Text,
  outcome: LookupOutcome
}

LookupOutcome =
    Found { matches: [LookupEntry], truncated: Bool }
  | NotFound
  | Ambiguous { matches: [LookupEntry], truncated: Bool }
  | Rejected { diagnostic: Text }

LookupEntry {
  name: Text,
  defining_module: Maybe Text,
  kind: value | class_method | record_selector | constructor | type | coercion,
  signature_or_declaration: Text,
  origin: module_export | live_binding,
  quality: exact | usable
}
```

Name lookup may use `Ambiguous`. Type lookup returns ordered `Found` matches.
`Rejected` covers that query's parse/kind/typecheck failure. Infrastructure
failure remains failure of the worker batch; it is not rendered as a user
query rejection.

`quality` and `origin` are closed enums and own ordering/control flow. Display
text never drives behavior. Exact matches precede usable matches; the remaining
order is stable by callable occurrence, defining module, then rendered type.

## Scope and compilation

`actor_compile_view` remains the only authority for preamble, imports, current
Lib.G heads and injected current Val.G modules. Lookup uses the same immutable
prepared view as existing inspection.

A batch may compile one isolated inspection module per type query so one bad
query cannot suppress its siblings. It must never compile per candidate. Each
compiled type query enumerates `globalRdrEnvElts` from its own checked target
and matches resolved `Id` types in memory. Name queries keep existing name
inspection semantics.

Before typechecking, normalize the GHC-parsed type AST:

- preserve each named query variable and its repeated occurrences;
- replace each anonymous wildcard node with a distinct fresh query variable;
- explicitly quantify all free query variables;
- preserve qualification, constraints and source spans where GHC permits.

Do not normalize by editing source text. Raw partial-signature holes zonk to
`ZonkAny` and are not matcher variables, as
`probes/TypeQuery.hs` demonstrates.

## Coordinator patch obligations

The coordinator owns and applies:

1. new extractor request field/CLI encoding for `SearchType`;
2. `Main.hs` dispatch and the chosen explicit TPINSP migration;
3. actor-side trusted lookup request, execution-ID/retry behavior and shared
   built-in registration/catalog;
4. common documentation-test harness wiring.

Lookup children return focused commits for their owned modules plus a compact
patch obligation naming every coordinator-owned consumer. They do not edit
those shared files.

## First integration assertions

- A batch `[valid name, ill-kinded type, valid type]` returns three ordered
  outcomes and both successes.
- `:: Response result -> Await (Settlement result)` returns
  `awaitSettled`, whose current signature is checked rather than copied from
  prose.
- Repeated named variables differ from independent `_` wildcards.
- Candidate enumeration includes the current imported live binding and excludes
  its shadowed predecessor.
- A returned callable name is accepted in the next real Haskell submission.
