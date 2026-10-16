import Jev.Host (jevTransport)
import Tidepool.Aeson.Value (Value (Bool), object, (.=))

do
  startupCurrent <- C.getContext
  C.ContextStaged _ <- C.stageContext
    (C.contextPatch [C.replaceText (startupTarget startupCurrent) "startup-must-not-publish"])
  Right (Bool True) <- jevTransport (object ["startup_refresh_staged" .= True])
  error "startup refresh failure control" >> pure True
