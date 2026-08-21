//! The Event effect — typed repository events (PRD 19 lane L4), the second
//! effect to retire a `Wt*`/`Ev*`-family hand-written wire block (§11 of the
//! scaffold doc; Worktree was the first).
//!
//! **The motivating bug lives here.** `RepoEventAwait` was added to registry 1
//! (`tidepool-mcp/src/effect_defs.rs`) and missed in registry 4
//! (`tidepool-harness/src/engine.rs`'s `classify_hole`) — the defect PRD 22
//! exists to make structurally impossible. `classify_hole` was already patched
//! to cover all seven Event verbs before this lane (see
//! `22-p3-event-survey.md`); every verb below carries
//! `HandlingClass::OuterDispatch(OuterEffect::RepoEvent)`, the SAME class
//! `RepoEventAwait` and its six siblings already share at the hand-routed call
//! site — this is what makes a future eighth verb fail GENERATION rather than
//! silently falling through to `HoleRouting::Ask` if its class is forgotten.
//!
//! **Two things this lane needed that Worktree's lane did not, both because
//! `Watch`/`HeadChangeKind`/`HeadChangeReceipt`/`CommitReceipt` name Worktree's
//! OWN types (`WorktreeId`, `GitOid`, `BranchName`) rather than only their own
//! effect's:**
//!
//! 1. [`crate::schema::Effect::foreign_types`] — a small (Haskell name, Rust
//!    wire name) table for a NAMED type this effect's `type_defs` reference but
//!    do not themselves declare. All Haskell decls land in one generated
//!    `Tidepool.Effects` module regardless of which effect owns them, so the
//!    HASKELL side needs nothing new; only the WIRE Rust emitter's per-effect
//!    `wire_rust_of` lookup needed a fallback.
//! 2. `HsType::Value` as a wire record field type (`RepositoryEvent`'s
//!    `ObservedMessage EventId Int Value`), rendering as `serde_json::Value` —
//!    the same spelling `AgCyclePayload`/`AgAgentStep` already use for a
//!    ret-only JSON payload.
//!
//! **Type-def relocation, not just helper relocation.** `Event a` and
//! `Observed a` are genuinely polymorphic (a type parameter, and for `Event` a
//! function-typed field), and `instance Functor Event where …` is a typeclass
//! instance — none of that has a schema vocabulary, and none should grow one
//! for a single use. All three stay hand-written, relocated into
//! `haskell/lib/Tidepool/Event.hs` alongside the eighteen non-representable
//! helpers (§11.9's lever, applied to type declarations for the first time
//! rather than only to helpers). No `ToJSON` instance exists anywhere in the
//! current registry for any Event type, so every representable `TypeDef` here
//! carries `json: JsonInstance::None` — simpler than Worktree's seven.
//!
//! **Helper representability.** Twenty-two authored names, all `raw` in the
//! hand-written registry. FOUR are thin wrappers over one verb and are
//! described here (`awaitSubscriptionRaw`, `mailboxNew`, `mailboxSend`,
//! `mailboxDrop` — PRD 20 S1-L4 wave 2's capability-mailbox trio plus the
//! blocking-wait primitive). The other EIGHTEEN — `commit`, `projectCommit`,
//! `headChanged`, `projectHead`, `(<|>)`, `pumpEff`, `drainSubscription`,
//! `withHandler`, `eventIdOf`, `firstMatch`, `nextEvent`, `awaitFirst`, `after`,
//! `projectTick`, `mailbox`, `projectMailbox`, `asyncDone`, `projectAsyncDone`
//! — are constructor applications, `case` matches over a sum's variants, `do`
//! blocks, or recursive functions: none is a thin single-verb wrapper, so none
//! is representable under [`crate::schema::HelperBody`]'s closed shapes. They
//! are DEFINITIONS in `haskell/lib/Tidepool/Event.hs`, reached through this
//! effect's `extra_imports` row exactly as Worktree's ten relocated helpers
//! are reached through `import Tidepool.Worktree`.
//!
//! **No adapter module.** Every Event `TypeDef.domain` is `None`: unlike
//! Worktree's `WorktreeReceipt`/`WorktreeSpec`, there is no separate Rust
//! DOMAIN type for a repository event — `SubscriptionRegistry`
//! (`tidepool-handlers/src/handlers/event.rs`) uses the `Ev*` wire structs
//! directly as its working representation. `has_adapters` stays false, and no
//! `tidepool-handlers/src/generated/event_adapters.rs` is emitted.
//!
//! **`Translate.hs` (registry 3) is untouched.** Grep across the whole file
//! finds zero references to `RepoEvent`/`Event`/any Event type — confirmed in
//! `22-p3-event-survey.md`. `vsMisShapeIsError` (PRD 22 open question 4) is
//! N/A to this lane; Event's rows never reach it.

use crate::hs::HsType;
use crate::schema::{
    Arg, Effect, ErrorAdt, ErrorField, ErrorVariant, HandlingClass, Helper, HelperBody,
    IdentityPayload, JsonInstance, OuterEffect, RecordField, RustBinding, SumVariant, TypeDef,
    TypeShape, Validation, Verb, WireDerives,
};
use crate::types::WireDerive::{
    Clone as DClone, Copy as DCopy, Debug as DDebug, Eq as DEq, FromCore as DFromCore,
    PartialEq as DPartialEq, ToCore as DToCore,
};

/// The derive set most Event wire types share: no `Copy` (several carry a
/// `String`-backed `WorktreeId`/`GitOid`/`BranchName` transitively).
const WIRE: WireDerives = WireDerives(&[DToCore, DFromCore, DClone, DDebug, DPartialEq, DEq]);
/// …plus `Copy`, for the payload-free/scalar-only shapes.
const WIRE_COPY: WireDerives =
    WireDerives(&[DToCore, DFromCore, DClone, DCopy, DDebug, DPartialEq, DEq]);
/// `SubscriptionId` alone additionally derives `PartialOrd, Ord, Hash` — it is
/// used as a map/set key in `SubscriptionRegistry`.
const WIRE_ID_ORD_HASH: WireDerives = WireDerives(&[
    DToCore,
    DFromCore,
    DClone,
    DCopy,
    DDebug,
    DPartialEq,
    DEq,
    crate::types::WireDerive::PartialOrd,
    crate::types::WireDerive::Ord,
    crate::types::WireDerive::Hash,
]);
/// `EventId` alone additionally derives `PartialOrd, Ord` (no `Hash`).
const WIRE_ID_ORD: WireDerives = WireDerives(&[
    DToCore,
    DFromCore,
    DClone,
    DCopy,
    DDebug,
    DPartialEq,
    DEq,
    crate::types::WireDerive::PartialOrd,
    crate::types::WireDerive::Ord,
]);
/// `RepositoryEvent` alone: RET-ONLY (never decoded from Haskell — it appears
/// solely as `ret "[RepositoryEvent]"`, never in an `args { .. }` clause), so
/// `FromCore` would be an unreachable impl, not a capability, and it carries a
/// `serde_json::Value` field (`ObservedMessage`'s payload), which has no `Eq`.
const WIRE_RET_ONLY: WireDerives = WireDerives(&[DToCore, DClone, DDebug, DPartialEq]);

/// `data X = X Int` — an opaque runtime-minted identity. Both Event identities
/// carry no string policy (`IdentityPayload::Int` requires
/// `Validation::None` — [`TypeDef::validate`]) and neither has a domain type of
/// its own: `SubscriptionRegistry` mints and compares the wire struct directly.
fn identity(
    name: &'static str,
    wire_rust: &'static str,
    derives: WireDerives,
    doc: &'static [&'static str],
) -> TypeDef {
    TypeDef {
        name,
        wire_rust: Some(wire_rust),
        shape: TypeShape::Identity {
            payload: IdentityPayload::Int,
            hs_binder: "i",
            rust_field: "raw",
            validation: Validation::None,
        },
        json: JsonInstance::None,
        derives,
        domain: None,
        doc,
    }
}

/// The Event effect, completely.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn event() -> Effect {
    Effect {
        name: "RepoEvent",
        handler: "RepoEventHandler",
        handler_module: "event",
        req_enum: "RepoEventReq",
        decl_fn: "event_decl",
        description: &[
            "Typed repository events. `commit tree` and `headChanged tree` are event ",
            "DESCRIPTIONS — values you can build, `fmap`, and merge with `<|>` before ",
            "anything is registered. `withHandler event handler body` makes one live ",
            "for exactly the extent of its lexical body: it registers without ",
            "blocking, never replays events older than the registration, invokes the ",
            "handler in the SAME effect row as the surrounding code (so it may send a ",
            "typed message, spawn a reviewer, or ask the operator — and may itself ",
            "suspend), runs one handler at a time per subscription with later ",
            "observations queued in observation order, and on exit closes intake, ",
            "drains, then unregisters. Handler failure fails the enclosing scope. ",
            "Queue overflow fails loudly — commits are never silently dropped. ",
            "`nextEvent event` blocks until the FIRST matching observation (or ",
            "forever): subscribe, block-await, unsubscribe — the one-shot sibling ",
            "of `withHandler`, no caller-supplied timeout. `after ms` is a ",
            "one-shot deadline event, `ms` milliseconds from the moment it is ",
            "SUBSCRIBED (not from the `after` call itself), that fires exactly one ",
            "`Tick`, so `nextEvent (someEvent <|> after ms)` reads as an ordinary ",
            "select with a timeout branch.",
        ],
        prompt_card: None,
        type_params: &[],
        default_row_args: &[],
        helpers_row_polymorphic: false,
        // Eighteen of the twenty-two authored names are not schema-representable
        // (module doc above) and are DEFINITIONS in `haskell/lib/Tidepool/Event.hs`
        // — including `Event`/`Observed`/the `Functor Event` instance, which are
        // TYPE declarations this row used to carry inline. This row is what makes
        // the relocation invisible to an eval author: a row carrying RepoEvent
        // imports that module, so all twenty-two names resolve exactly as they did
        // when the generated `Tidepool.Effects` defined them.
        extra_imports: &["import Tidepool.Event"],
        type_defs: type_defs(),
        foreign_types: &[
            ("WorktreeId", "WtWorktreeId"),
            ("GitOid", "WtGitOid"),
            ("BranchName", "WtBranchName"),
        ],
        // Typed per-verb failure (#335): overflow, an unknown subscription/
        // mailbox, a lost/failed source, or an oversized timeout are DATA an
        // author cases on, not an eval abort.
        errors: Some(errors()),
        verbs: verbs(),
        helpers: helpers(),
    }
}

/// The eight supporting declarations, in the order they are emitted — every
/// `data` decl in schema order, then (none here — see the module doc) every
/// `ToJSON` instance, then the derived error ADT. Reproduces
/// `event_effect_def!`'s `type_defs` list exactly, MINUS `Observed a`/`Event
/// a`/`instance Functor Event`, which relocate — see the module doc.
fn type_defs() -> Vec<TypeDef> {
    vec![
        identity(
            "EventId",
            "EvEventId",
            WIRE_ID_ORD,
            &[
                "Haskell `EventId` — opaque RUNTIME identity, minted once per",
                "reconciliation pass. A normal commit's `commit` and `headChanged`",
                "observations share one, which is how a consumer tells two views of one",
                "change from two changes.",
            ],
        ),
        identity(
            "SubscriptionId",
            "EvSubscriptionId",
            WIRE_ID_ORD_HASH,
            &["Haskell `SubscriptionId` — one live `withHandler` registration."],
        ),
        TypeDef {
            name: "Watch",
            wire_rust: Some("EvWatch"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "WatchCommit",
                        fields: vec![HsType::Named("WorktreeId")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "WatchHead",
                        fields: vec![HsType::Named("WorktreeId")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "WatchDeadline",
                        fields: vec![HsType::Int],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "WatchAsync",
                        fields: vec![HsType::Int],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "WatchMailbox",
                        fields: vec![HsType::Int],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &[
                "Haskell `Watch` — one (worktree, kind) pair a subscription observes, or a",
                "one-shot deadline. `<|>` concatenates watches, so a merged `Event` is ONE",
                "subscription over several watches rather than several subscriptions.",
                "`WatchDeadline` carries a RELATIVE millisecond duration: the runtime fixes",
                "the absolute deadline at `subscribe()` time, `now + ms`. `WatchAsync`/",
                "`WatchMailbox` name no worktree either — a raw `Int` rather than a",
                "newtype, since this row must stand alone without `Green`.",
            ],
        },
        TypeDef {
            name: "HeadChangeKind",
            wire_rust: Some("EvHeadChangeKind"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "Advanced",
                        fields: vec![HsType::list(HsType::Named("GitOid"))],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "Amended",
                        fields: vec![HsType::Named("GitOid"), HsType::Named("GitOid")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "Rewritten",
                        fields: vec![HsType::list(HsType::Tuple(vec![
                            HsType::Named("GitOid"),
                            HsType::Named("GitOid"),
                        ]))],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "Rewound",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "Switched",
                        fields: vec![],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "UnknownChange",
                        fields: vec![],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &[
                "Haskell `HeadChangeKind`. `UnknownChange` is a correct answer, not a",
                "failure: inventing `Advanced` for what was actually a reset would send a",
                "child rebasing onto a commit that no longer means what the claim said.",
            ],
        },
        TypeDef {
            name: "HeadChangeReceipt",
            wire_rust: Some("EvHeadChangeReceipt"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "headWorktree",
                        rust_name: "head_worktree",
                        ty: HsType::Named("WorktreeId"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "oldHead",
                        rust_name: "old_head",
                        ty: HsType::maybe(HsType::Named("GitOid")),
                        doc: &["`None` on the first observation of a worktree that had no recorded head."],
                    },
                    RecordField {
                        hs_name: "newHead",
                        rust_name: "new_head",
                        ty: HsType::Named("GitOid"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "kind",
                        rust_name: "kind",
                        ty: HsType::Named("HeadChangeKind"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "headBranch",
                        rust_name: "head_branch",
                        ty: HsType::maybe(HsType::Named("BranchName")),
                        doc: &["`None` on a detached HEAD."],
                    },
                    RecordField {
                        hs_name: "observedAtMs",
                        rust_name: "observed_at_ms",
                        ty: HsType::Int,
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["Haskell `HeadChangeReceipt`."],
        },
        TypeDef {
            name: "CommitReceipt",
            wire_rust: Some("EvCommitReceipt"),
            shape: TypeShape::Record {
                fields: vec![
                    RecordField {
                        hs_name: "commitWorktree",
                        rust_name: "commit_worktree",
                        ty: HsType::Named("WorktreeId"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "oid",
                        rust_name: "oid",
                        ty: HsType::Named("GitOid"),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "parents",
                        rust_name: "parents",
                        ty: HsType::list(HsType::Named("GitOid")),
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "subject",
                        rust_name: "subject",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "author",
                        rust_name: "author",
                        ty: HsType::Text,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "committedAtMs",
                        rust_name: "committed_at_ms",
                        ty: HsType::Int,
                        doc: &[],
                    },
                    RecordField {
                        hs_name: "files",
                        rust_name: "files",
                        ty: HsType::list(HsType::Text),
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE,
            domain: None,
            doc: &["Haskell `CommitReceipt`."],
        },
        TypeDef {
            name: "Tick",
            // The Rust struct name differs from the Haskell type name — the
            // hand-written block already spelled it `EvTickReceipt`, not
            // `EvTick`, so `#[core(name = "Tick")]` is needed here too
            // (`TypeDef::needs_core_name` fires exactly on that mismatch).
            wire_rust: Some("EvTickReceipt"),
            shape: TypeShape::Record {
                fields: vec![RecordField {
                    hs_name: "firedAtMs",
                    rust_name: "fired_at_ms",
                    ty: HsType::Int,
                    doc: &[],
                }],
            },
            json: JsonInstance::None,
            derives: WIRE_COPY,
            domain: None,
            doc: &[
                "Haskell `Tick` — the payload a fired deadline watch delivers. Carries the",
                "wall-clock moment the runtime observed it as due; the deadline the caller",
                "asked for lives only in the `WatchDeadline` that fired.",
            ],
        },
        TypeDef {
            name: "RepositoryEvent",
            wire_rust: Some("EvRepositoryEvent"),
            shape: TypeShape::Sum {
                variants: vec![
                    SumVariant {
                        ctor: "ObservedCommit",
                        fields: vec![HsType::Named("EventId"), HsType::Named("CommitReceipt")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ObservedHeadChange",
                        fields: vec![
                            HsType::Named("EventId"),
                            HsType::Named("HeadChangeReceipt"),
                        ],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ObservedTick",
                        fields: vec![HsType::Named("EventId"), HsType::Named("Tick")],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ObservedAsyncDone",
                        fields: vec![HsType::Named("EventId"), HsType::Int],
                        doc: &[],
                    },
                    SumVariant {
                        ctor: "ObservedMessage",
                        fields: vec![HsType::Named("EventId"), HsType::Int, HsType::Value],
                        doc: &[],
                    },
                ],
            },
            json: JsonInstance::None,
            derives: WIRE_RET_ONLY,
            domain: None,
            doc: &[
                "Haskell `RepositoryEvent` — one reconciled fact as it crosses the boundary.",
                "The `EventId` rides on the wire rather than being minted per view, because",
                "the SHARING is the information. `ObservedTick` is never broadcast — a fired",
                "deadline is queued directly onto the ONE subscription that armed it.",
                "`ObservedAsyncDone` carries only the settled thread's `Int` id, never its",
                "result. `ObservedMessage` carries a mailbox `Int` and a bare JSON payload;",
                "the coalesce key does not ride the wire.",
                "Ret-only (never decoded from Haskell) — it appears exclusively as",
                "`ret \"[RepositoryEvent]\"`, never in an `args { .. }` clause, so `FromCore`",
                "here would be an unreachable impl, not a capability.",
            ],
        },
    ]
}

fn errors() -> ErrorAdt {
    ErrorAdt {
        name: "EventError",
        variants: vec![
            ErrorVariant {
                ctor: "EventQueueOverflow",
                fields: vec![
                    ErrorField {
                        name: "overflowSub",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                    ErrorField {
                        name: "dropped",
                        ty: HsType::Int,
                        rust: RustBinding::Derived,
                    },
                ],
                doc: "the per-subscription queue bound was exceeded — the scope fails rather than dropping commits",
            },
            ErrorVariant {
                ctor: "EventUnknownSubscription",
                fields: vec![ErrorField {
                    name: "unknownSub",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                doc: "no such live subscription (already unregistered)",
            },
            ErrorVariant {
                ctor: "EventSourceLost",
                fields: vec![ErrorField {
                    name: "lostDetail",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                }],
                doc: "a watched worktree is no longer observable",
            },
            ErrorVariant {
                ctor: "EventSourceFailed",
                fields: vec![ErrorField {
                    name: "failedDetail",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                }],
                doc: "reconciliation against git failed",
            },
            ErrorVariant {
                ctor: "EventUnknownMailbox",
                fields: vec![ErrorField {
                    name: "unknownMailbox",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                }],
                doc: "no such live mailbox — never minted, or already dropped",
            },
        ],
    }
}

fn subscription_arg() -> Arg {
    Arg {
        name: "subscription",
        ty: HsType::Named("SubscriptionId"),
        rust: RustBinding::Bridged("EvSubscriptionId"),
    }
}

/// All seven verbs share ONE handling class: the same `OuterDispatch(RepoEvent)`
/// `RepoEventAwait` and its siblings already reach through the hand-routed
/// `classify_hole` match arm (`tidepool-harness/src/engine.rs`) — this is what
/// makes an eighth verb without a class fail GENERATION rather than silently
/// falling through to `HoleRouting::Ask`, the exact `RepoEventAwait` bug class.
fn verbs() -> Vec<Verb> {
    vec![
        Verb {
            ctor: "RepoEventSubscribe",
            method: "repo_event_subscribe",
            args: vec![Arg {
                name: "watches",
                ty: HsType::list(HsType::Named("Watch")),
                rust: RustBinding::Path("Vec<tidepool_bridge_effects::EvWatch>"),
            }],
            ret: HsType::Named("SubscriptionId"),
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        Verb {
            ctor: "RepoEventDrain",
            method: "repo_event_drain",
            args: vec![subscription_arg()],
            ret: HsType::list(HsType::Named("RepositoryEvent")),
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        // BLOCKS at the handler until the subscription has >= 1 observation or
        // `timeoutMs` elapses (negative == no deadline). This exact verb is the
        // motivating bug — see the module doc.
        Verb {
            ctor: "RepoEventAwait",
            method: "repo_event_await",
            args: vec![
                subscription_arg(),
                Arg {
                    name: "timeoutMs",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
            ],
            ret: HsType::list(HsType::Named("RepositoryEvent")),
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        Verb {
            ctor: "RepoEventUnsubscribe",
            method: "repo_event_unsubscribe",
            args: vec![subscription_arg()],
            ret: HsType::Unit,
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        // Capability mailboxes (PRD 20 S1-L4 wave 2). A mailbox IS an event
        // source, so it lives on RepoEvent rather than Green.
        Verb {
            ctor: "MailboxNew",
            method: "mailbox_new",
            args: vec![],
            ret: HsType::Int,
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        Verb {
            ctor: "MailboxSend",
            method: "mailbox_send",
            args: vec![
                Arg {
                    name: "mailbox",
                    ty: HsType::Int,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "key",
                    ty: HsType::Text,
                    rust: RustBinding::Derived,
                },
                Arg {
                    name: "payload",
                    ty: HsType::Value,
                    rust: RustBinding::JsonValue,
                },
            ],
            ret: HsType::Unit,
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
        Verb {
            ctor: "MailboxDrop",
            method: "mailbox_drop",
            args: vec![Arg {
                name: "mailbox",
                ty: HsType::Int,
                rust: RustBinding::Derived,
            }],
            ret: HsType::Unit,
            errors: Some("EventError"),
            handling: HandlingClass::OuterDispatch(OuterEffect::RepoEvent),
            extract: None,
        },
    ]
}

/// The FOUR representable helpers: three thin one-verb wrappers over the
/// capability-mailbox trio, plus the blocking-wait primitive `nextEvent`/
/// `awaitFirst` build on. Eighteen more live in
/// `haskell/lib/Tidepool/Event.hs` as DEFINITIONS — the per-helper verdict is
/// the module doc above and `22-p3-event-survey.md`.
fn helpers() -> Vec<Helper> {
    vec![
        Helper {
            name: "awaitSubscriptionRaw",
            ctor: Some("RepoEventAwait"),
            doc: &[
                "Block until `sub` has queued at least one observation, or",
                "`timeoutMs` elapses (negative blocks with no deadline). An elapsed",
                "timeout is an EMPTY list — distinguishable from a real batch, never",
                "an error; poison/source-loss still fail via the `Either`.",
            ],
            body: HelperBody::Applied(&["sub", "timeoutMs"]),
        },
        Helper {
            name: "mailboxNew",
            ctor: Some("MailboxNew"),
            doc: &[
                "Mint a fresh mailbox: an event source only the caller (and whoever",
                "it hands the id to) can send into.",
            ],
            body: HelperBody::Nullary,
        },
        Helper {
            name: "mailboxSend",
            ctor: Some("MailboxSend"),
            doc: &[
                "Send never blocks: append and return. A burst of sends sharing",
                "`key` coalesces to the LAST payload.",
            ],
            body: HelperBody::Applied(&["mid", "key", "payload"]),
        },
        Helper {
            name: "mailboxDrop",
            ctor: Some("MailboxDrop"),
            doc: &[
                "Drop a mailbox. A later send against it is",
                "`Left (EventUnknownMailbox _)`.",
            ],
            body: HelperBody::Pointfree,
        },
    ]
}
