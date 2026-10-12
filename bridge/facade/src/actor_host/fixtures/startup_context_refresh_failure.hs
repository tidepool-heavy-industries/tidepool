import Jev.Host (jevTransport)
import Tidepool.Aeson.Value (Value (Bool), object, (.=))

do
  startupCurrent <- C.getContext
  C.putContext (over C.contextBlocks (map (startupReplace "startup-must-not-publish")) startupCurrent)
  Right (Bool True) <- jevTransport (object ["startup_refresh_staged" .= True])
  error "startup refresh failure control" >> pure True
