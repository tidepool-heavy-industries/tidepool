# Notification effect/runtime wire scaffold

Parent seed a33ed927 supplies compiled one-shot host handoffs and explicit
unsupported host consumer. No native presentation claim.

Generated effect Notifications:
- `NotifyWith :: (Int, Int) -> Text -> Notifications (Either NotificationError ((Int, Int), (Int, Int), Text, Int))`
- `PollNotificationWith :: ((Int, Int), (Int, Int), Text, Int) -> Notifications (Either NotificationError NotificationState)`
- Rust decoder: NotificationsReq in generated/notifications.rs.

Receipt tuple is issuing owner, target, existing inbox key, sequence. Facade owns
abstract NotificationReceipt wrapping tuple; notify/pollNotification are Member
Notifications wrappers in existing internal Agent module (which already owns
AgentRef). Effect declaration/schema does not depend on that wrapper or module.
Errors: NotificationUnauthorized, NotificationUnavailable,
NotificationInvalidReceipt, NotificationAdmissionUnconfirmed Text,
NotificationStorageFailure Text. States: NotificationAccepted,
NotificationPresented, NotificationUnconfirmed. Keep current Rust host-facing
error/state names via explicit ToCore constructor mappings; no host API rename.

Schema/Haskell worker owns protocol and all generated consumers, Haskell facade
and Role effect registration. Runtime TL owns Rust role/start effects, notification
handoff construction/validation and resident interpreter. Append Notifications to
existing effect rows so unrelated union indices do not change; all roles that
currently support Replies also support one-way Notifications. Runtime authority
remains same-session exact live target resolution (like request); poll requires
exact issuing owner plus host inbox/retained-row authority. Never mint owner from
receipt fields or modify the request registry.
