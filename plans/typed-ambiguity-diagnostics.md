# Typed ambiguity diagnostics: pending compiler-owner repair

Replace the rendered-diagnostic parser used by same-cell ambiguity retry with
typed GHC candidate identities. The existing compiler diagnostic owner must
carry that evidence through the report and Rust decoder before replacing the
retry predicate; rendered text remains human advice.

## Current production boundary

`bridge/haskell/src/Tidepool/DiagJson.hs` already recognizes GHC's
`TcRnAmbiguousName _ _ candidates`, after unwrapping
`TcRnMessageWithInfo`/`TcRnMessageDetailed` and `TcRnWithHsDocContext`.
`ambiguousOccurrenceHint` projects those typed candidates to a human hint.
The exported `Diag` and JSON report retain only span, severity, and rendered
message, losing the category and candidate identities.

`exomonad/actor/src/resident_workbench.rs` calls
`runtime::session::turn::same_cell_value_collisions` in whole-cell preflight and
admitted declaration checking. That function searches rendered English
`Ambiguous occurrence` text, module prefixes, and field/method wording. Its
result changes a generated import's hiding list before one more compiler check.
The retry occurs before installation or authored execution; it is not an effect
replay or uncertain-write recovery. Typed ownership still should decide it.

## Proposed owner contract

Extend the existing `Diag`/compiler report with optional typed source diagnostic
context. Mint an ambiguity record only from the GHC category above, using the
existing compiler identity encoder rather than module-name parsing. Retain the
ambiguous occurrence and each candidate's full unit/module identity, namespace,
occurrence, and ordinary-value versus record-field/class-method membership. The
member parent must be retained where GHC supplies it. Keep rendered text and the
qualification hint as human advice.

The Rust compiler diagnostic decoder should preserve this typed record through
`CompileError::Diagnostics` and `CellCheckFailure`. The existing same-cell retry
owner should compare the exact current and candidate declaration identities,
accept only ordinary value candidates for the same occurrence, and reject any
additional owner or type/member candidate. Preserve the existing bounded retry
count and checking-before-execution order. Missing typed context must not admit
an automatic hiding retry. Do not extend the rendered-string parser.

This is descriptive compiler evidence, not source or runtime authority. A new
registry, diagnostic supervisor, or independent identity issuer is unnecessary.

## Required proof before replacing the parser

Use genuine GHC diagnostics for an ordinary value redeclared and used in one
cell, a record field, a class method, and an extra package/module candidate.
Show that the allowed ordinary-value case retries once and that all other cases
remain source refusals with their typed candidates. Change rendered wording in
a Rust fixture while keeping typed context unchanged; retry behavior must stay
unchanged. Missing context must remain refused. Compile the owning worker and
its downstream Rust decoder and execute the real compiler-backed cases with
matching producer binaries.
