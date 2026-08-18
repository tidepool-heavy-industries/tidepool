//! The Haskell-side independent pin for the Event (`RepoEvent`) effect — the
//! same proof `worktree_haskell_contract.rs` gives Worktree's `type_defs`/
//! `constructor_signatures`/`helpers`, extended with Event's OWN finding:
//! `Event a`/`Observed a`/`instance Functor Event` are genuinely polymorphic
//! and have no schema vocabulary at all (Worktree's non-representable names
//! were all HELPERS; here two TYPE declarations and a typeclass instance
//! relocate too) — see `tidepool-protocol/src/effects/event.rs`'s module doc
//! and `plans/self-iterating-harness/22-p3-event-survey.md`.
//!
//! These literal strings are HAND-TRANSCRIBED from `event_effect_def!`
//! (`tidepool-mcp/src/effect_defs.rs`) as it stood before this lane's flip —
//! not copied from the schema's own rendering — so a rendering bug that
//! produces SOME plausible output is still caught.

use tidepool_protocol::effects::event::event;

#[test]
fn event_schema_is_valid() {
    if let Err(problems) = event().validate() {
        panic!("Event schema is invalid:\n  {}", problems.join("\n  "));
    }
}

#[test]
fn event_type_def_texts_are_pinned() {
    let ev = event();

    assert_eq!(
        ev.type_def_texts(),
        vec![
            "data EventId = EventId Int deriving (Show, Eq)".to_string(),
            "data SubscriptionId = SubscriptionId Int deriving (Show, Eq)".to_string(),
            "data Watch = WatchCommit WorktreeId | WatchHead WorktreeId | WatchDeadline Int | \
             WatchAsync Int | WatchMailbox Int deriving (Show, Eq)"
                .to_string(),
            "data HeadChangeKind = Advanced [GitOid] | Amended GitOid GitOid | Rewritten \
             [(GitOid, GitOid)] | Rewound | Switched | UnknownChange deriving (Show, Eq)"
                .to_string(),
            "data HeadChangeReceipt = HeadChangeReceipt { headWorktree :: WorktreeId, oldHead \
             :: Maybe GitOid, newHead :: GitOid, kind :: HeadChangeKind, headBranch :: Maybe \
             BranchName, observedAtMs :: Int } deriving (Show, Eq)"
                .to_string(),
            "data CommitReceipt = CommitReceipt { commitWorktree :: WorktreeId, oid :: GitOid, \
             parents :: [GitOid], subject :: Text, author :: Text, committedAtMs :: Int, files \
             :: [Text] } deriving (Show, Eq)"
                .to_string(),
            "data Tick = Tick { firedAtMs :: Int } deriving (Show, Eq)".to_string(),
            "data RepositoryEvent = ObservedCommit EventId CommitReceipt | ObservedHeadChange \
             EventId HeadChangeReceipt | ObservedTick EventId Tick | ObservedAsyncDone EventId \
             Int | ObservedMessage EventId Int Value deriving (Show, Eq)"
                .to_string(),
            concat!(
                "data EventError = EventQueueOverflow Int Int | EventUnknownSubscription Int | ",
                "EventSourceLost Text | EventSourceFailed Text | EventUnknownMailbox Int | ",
                "EventBadTimeout Int deriving (Show, Eq)\n",
                "instance ToJSON EventError where\n",
                "  toJSON e = case e of\n",
                "    EventQueueOverflow overflowSub dropped -> object [\"tag\" .= \
                 (\"EventQueueOverflow\" :: Text), \"overflowSub\" .= overflowSub, \"dropped\" \
                 .= dropped]\n",
                "    EventUnknownSubscription unknownSub -> object [\"tag\" .= \
                 (\"EventUnknownSubscription\" :: Text), \"unknownSub\" .= unknownSub]\n",
                "    EventSourceLost lostDetail -> object [\"tag\" .= (\"EventSourceLost\" :: \
                 Text), \"lostDetail\" .= lostDetail]\n",
                "    EventSourceFailed failedDetail -> object [\"tag\" .= \
                 (\"EventSourceFailed\" :: Text), \"failedDetail\" .= failedDetail]\n",
                "    EventUnknownMailbox unknownMailbox -> object [\"tag\" .= \
                 (\"EventUnknownMailbox\" :: Text), \"unknownMailbox\" .= unknownMailbox]\n",
                "    EventBadTimeout badTimeoutMs -> object [\"tag\" .= (\"EventBadTimeout\" \
                 :: Text), \"badTimeoutMs\" .= badTimeoutMs]\n",
            )
            .to_string(),
        ]
    );
}

#[test]
fn event_constructor_signatures_are_pinned() {
    let ev = event();

    assert_eq!(
        ev.constructor_signatures(),
        vec![
            "RepoEventSubscribe :: [Watch] -> RepoEvent (Either EventError SubscriptionId)",
            "RepoEventDrain :: SubscriptionId -> RepoEvent (Either EventError [RepositoryEvent])",
            "RepoEventAwait :: SubscriptionId -> Int -> RepoEvent (Either EventError \
             [RepositoryEvent])",
            "RepoEventUnsubscribe :: SubscriptionId -> RepoEvent (Either EventError ())",
            "MailboxNew :: RepoEvent (Either EventError Int)",
            "MailboxSend :: Int -> Text -> Value -> RepoEvent (Either EventError ())",
            "MailboxDrop :: Int -> RepoEvent (Either EventError ())",
        ]
    );
}

#[test]
fn event_helper_texts_are_pinned() {
    let ev = event();

    assert_eq!(
        ev.helper_texts(),
        vec![
            concat!(
                "-- | Block until `sub` has queued at least one observation, or\n",
                "-- `timeoutMs` elapses (negative blocks with no deadline). An elapsed\n",
                "-- timeout is an EMPTY list — distinguishable from a real batch, never\n",
                "-- an error; poison/source-loss still fail via the `Either`.\n",
                "awaitSubscriptionRaw :: SubscriptionId -> Int -> M (Either EventError \
                 [RepositoryEvent])\n",
                "awaitSubscriptionRaw sub timeoutMs = send (RepoEventAwait sub timeoutMs)",
            ),
            concat!(
                "-- | Mint a fresh mailbox: an event source only the caller (and whoever\n",
                "-- it hands the id to) can send into.\n",
                "mailboxNew :: M (Either EventError Int)\n",
                "mailboxNew = send MailboxNew",
            ),
            concat!(
                "-- | Send never blocks: append and return. A burst of sends sharing\n",
                "-- `key` coalesces to the LAST payload.\n",
                "mailboxSend :: Int -> Text -> Value -> M (Either EventError ())\n",
                "mailboxSend mid key payload = send (MailboxSend mid key payload)",
            ),
            concat!(
                "-- | Drop a mailbox. A later send against it is\n",
                "-- `Left (EventUnknownMailbox _)`.\n",
                "mailboxDrop :: Int -> M (Either EventError ())\n",
                "mailboxDrop = send . MailboxDrop",
            ),
        ]
    );
}

/// The eighteen helpers, plus the two type declarations and the `Functor`
/// instance, that are not schema-representable — see the module doc on
/// `tidepool-protocol/src/effects/event.rs`. Cross-checked here against the
/// schema's own helper list so a future edit that accidentally makes one of
/// these representable (and forgets to delete its `haskell/lib/Tidepool/
/// Event.hs` definition, producing an ambiguous occurrence) is caught by the
/// symmetric name NOT appearing among `helper_texts()`'s names.
#[test]
fn event_eighteen_helpers_are_not_schema_representable() {
    const NOT_REPRESENTABLE: &[&str] = &[
        "commit",
        "projectCommit",
        "headChanged",
        "projectHead",
        "<|>",
        "pumpEff",
        "drainSubscription",
        "withHandler",
        "eventIdOf",
        "firstMatch",
        "nextEvent",
        "awaitFirst",
        "after",
        "projectTick",
        "mailbox",
        "projectMailbox",
        "asyncDone",
        "projectAsyncDone",
    ];
    assert_eq!(NOT_REPRESENTABLE.len(), 18);

    let ev = event();
    let representable: Vec<&str> = ev.helpers.iter().map(|h| h.name).collect();
    for name in NOT_REPRESENTABLE {
        assert!(
            !representable.contains(name),
            "{name} was expected to stay non-representable (relocated to \
             haskell/lib/Tidepool/Event.hs), but the schema now describes it — \
             update this list AND delete the relocated definition in the same commit"
        );
    }
    assert_eq!(representable.len(), 4, "the four representable helpers");
}

#[test]
fn event_remaining_decl_fields_are_pinned() {
    let ev = event();

    assert_eq!(
        ev.description_text(),
        "Typed repository events. `commit tree` and `headChanged tree` are event \
         DESCRIPTIONS — values you can build, `fmap`, and merge with `<|>` before \
         anything is registered. `withHandler event handler body` makes one live \
         for exactly the extent of its lexical body: it registers without \
         blocking, never replays events older than the registration, invokes the \
         handler in the SAME effect row as the surrounding code (so it may send a \
         typed message, spawn a reviewer, or ask the operator — and may itself \
         suspend), runs one handler at a time per subscription with later \
         observations queued in observation order, and on exit closes intake, \
         drains, then unregisters. Handler failure fails the enclosing scope. \
         Queue overflow fails loudly — commits are never silently dropped. \
         `nextEvent event` blocks until the FIRST matching observation (or \
         forever): subscribe, block-await, unsubscribe — the one-shot sibling \
         of `withHandler`, no caller-supplied timeout. `after ms` is a \
         one-shot deadline event, `ms` milliseconds from the moment it is \
         SUBSCRIBED (not from the `after` call itself), that fires exactly one \
         `Tick`, so `nextEvent (someEvent <|> after ms)` reads as an ordinary \
         select with a timeout branch."
    );
    assert_eq!(ev.extra_imports, &["import Tidepool.Event"]);
    assert_eq!(
        ev.foreign_types,
        &[
            ("WorktreeId", "WtWorktreeId"),
            ("GitOid", "WtGitOid"),
            ("BranchName", "WtBranchName"),
        ]
    );
    assert!(ev.prompt_card.is_none());
    assert!(ev.type_params.is_empty());
    assert!(ev.default_row_args.is_empty());
    assert!(!ev.helpers_row_polymorphic);
    assert_eq!(ev.name, "RepoEvent");
    assert_eq!(ev.handler, "RepoEventHandler");
    assert_eq!(ev.handler_module, "event");
    assert_eq!(ev.req_enum, "RepoEventReq");
    assert_eq!(ev.decl_fn, "event_decl");
}
