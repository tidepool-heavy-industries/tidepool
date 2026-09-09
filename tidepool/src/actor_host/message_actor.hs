import Tidepool.Actor
owner <- actorContext
let messenger = (ActorDefinition
      { label = "message-handler"
      , effectProfile = ReadOnly
      , initialization = pure
      , behavior = \_ (recipient, payload) -> sendMessage recipient payload
      , onShutdown = const (pure ())
      } :: ActorDefinition (ActorContextInfo, Text) Maybe (Either NotificationError NotificationReceipt))
relay <- startActor messenger (owner, "e434: retain candidate; check digest")
