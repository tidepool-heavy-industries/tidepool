import Tidepool.Actor.Record (Handler)

let recordHandler :: Text -> Handler [Text] '[] (); recordHandler _ = pure ()
