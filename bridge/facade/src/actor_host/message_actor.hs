import Tidepool.Actor
let owner = me
let messenger = (ActorDefinition
      { label = "message-handler"
      , effectProfile = ReadOnly
      , initialization = pure
      , behavior = \_ (recipient, payload) -> sendMessage recipient payload
      , onShutdown = const (pure ())
      } :: ActorDefinition (AgentRef, Text) Maybe (Either NotificationError NotificationReceipt))
relay <- startActor messenger (owner, "e434: retain candidate; check digest")
