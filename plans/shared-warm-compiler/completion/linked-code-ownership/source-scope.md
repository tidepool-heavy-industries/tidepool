# Issued source scope versus original execution adjacency

Accepted bounded contract, 2026-10-22. Source investigation: `c5d39d0617`.
N owns the context field, constructors, projections, publication and persistence.
S and its Rust role worker own receipt-role acceptance and lexical traversal.
D reviews snapshot integration; T owns independent history coverage; root owns
integration. This contract is not compilation or execution evidence.

## Distinction and representation

`ExactDeclarationContext.facts.lexical` currently also stores the complete original
compiler/instance graph. `original_execution_context` composes consumed and
protected scaffold dependencies into that graph before
`from_authenticated_execution_support` retains it. Membership therefore cannot
establish permission to import or publish an owner as source.

Add a private `SourceLexicalScope` wrapper around the existing persistent
`ExactLexicalSnapshot`, carried by `DeclarationContextFacts`. It records only the
source surface issued by actual source, declaration, publication and retained-value
owners. It is not another inventory: two contexts may have identical compiler
support and execution adjacency but different source permissions. Reuse existing
immutable lexical facts, HAMTs and storage codecs. Do not clone or reconstruct the
full instance graph to derive this projection at each consumer.

Expose borrowed membership/import accessors and narrow owner-only issue, merge
and restriction operations. Do not expose a general constructor from arbitrary
rows. A source scope is bound to the exact selected compiler support of its context;
module names, import syntax, payload custody and interface equality alone cannot
issue it. `OriginalInstanceEnvironment::{Unknown,Complete,MissingOriginalOwners}`
describes instance evidence, not source authority, and must not select this policy.

## Issuers, preservers and clearers

| Boundary | Required treatment |
|---|---|
| `ExactDeclarationContext::new` / `extend` | Issue or replace source scope at the existing validated authored/join source boundary. Do not give arbitrary byte custody a source role. |
| Finite and World `SourceAdmissionBundle` projections | Extend scope from admitted lexical rows and their checked source edges. Retain compiler dependencies separately. |
| `ProgramSourceSupportRow` | Preserve its actual `admission.admits_lexical(owner)` proof; do not substitute its import adjacency or physical product. |
| `original_execution_context` / `from_authenticated_execution_support` | Carry the inherited narrow scope and genuine newly admitted source delta separately. The broad execution-graph argument never issues source scope. |
| `PublishedSourceOriginalSelection` | Publication consumes a genuine source proof for the requested root and source closure. A selected Original or direct edge in the full instance graph is insufficient. |
| `published_context_for_root` / `with_published_source_originals` | Restrict or merge the publication's issued source scope alongside its selected support. Keep the original instance graph for its separate consumer. |
| `extend_retained_value_artifacts` | Transfer the checked value's actual `source_lexical()` proof, not its complete original execution context. |
| `checked_value_interface_context` / `type_interfaces_from_support` | Produce empty source scope. TYPE acquisition cannot issue source permission. |
| `extend_interface_context` | Preserve the receiver's source scope; do not merge the incoming TYPE context's source permission. |
| Value-interface-only extension | Preserve already-issued source scope; interface requirements alone add no source grants. |
| Source-restricting projections | Restrict an already-issued scope against exact selected support. Never derive a new scope from selected inventory. |
| `WorldSourceCompilationBinding` / finite request / continuations | Borrow or carry the actual context's narrow proof. `CompilerWorldLexicalSurface` remains syntax plus explicit declaration/observation owners, not a source-grant census. |

Inherited-primary imports without matching genuine prior source proof remain
compiler dependencies and add no lexical delta. They are not automatically a
compilation refusal. When genuine prior proof exists, preserve that earlier scope
without reissuing it from the new receipt.

The receipt claim walk **and** `SourceAdmissionBundle::lexical_imports` must apply
this distinction. Finite source extension and World closure traversal must not
fall back to broad `context.lexical_graph()` / `facts.lexical` when source proof is
missing. Such a fallback would recreate the promotion after classification.

## Persistence and identity

Root approved these explicit format decisions:

- `ContextDescriptor` version **8 -> 9**, with a separate source-scope map root.
- `SurfaceSnapshot` remains **3**: its opaque `ContextRoot` reference shape is
  unchanged. Selection and publication-native formats remain unchanged unless
  their own layouts change.
- Exact context semantic encoding `TPEXACTCONTEXT` versions **12/13 -> 14/15**,
  adding independent source-scope facts in both production and oracle encoders.

Include scope in context equality, semantic observations, snapshot creation and
baseline equality. Retain structural sharing through the existing lexical fact
codec and map machinery. Old Context8 data refuses; there is no fallback that
interprets its broad lexical graph as source permission.

Hydration yields DATA. Expected-root recovery authenticates the exact selected
compiler support and validates all scope owners/edges against that support before
issuing a context carrying the scope. Check map keys against fact owners as for
existing lexical data. Bare recovery from inventory or a raw lexical vector cannot
reconstruct the missing source grant; it needs an explicit issuer-owned scope or
remains source-empty. Reopening must preserve the committed distinction even when
the full instance graph and selected product bytes are equal.

## Discriminating acceptance

Use actual producers and explicit nonzero premise observations:

1. A retained renderer/value captures a protected-only HOME dependency in its
   full original instance graph. A subsequent finite request and World request
   retain that dependency without acquiring lexical or publication permission.
2. Repeat through a secondary TYPE/compiler projection and cold Context9 reopen.
   Neither path may infer permission from the instance-environment enum, live
   ancestry, selected Original or broad graph membership.
3. Explicitly publish the same owner through its genuine source issuer, and show
   that the legitimate source scope survives the corresponding source-preserving
   projection and cold reopening. TYPE-only projection itself stays source-empty.
4. Two contexts with equal broader support/adjacency and different source scopes
   have different semantic identities; hydration preserves that distinction and
   exact shared immutable records where appropriate.
5. Wrong owner/interface, malformed or missing scope records, and Context8 input
   refuse at their actual owning boundaries. Rejection leaves the prior context
   unchanged and an honest continuation remains usable.

Compile changed production and test consumers, then execute these focused
histories before treating generated Display, ordinary G2 or frozen qualification
as accepted. This does not require a new context framework or graph subsystem.
