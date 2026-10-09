import qualified Tidepool.Aeson as HostJson
import qualified Data.Text as HostText
jsonSeen <-
  if input == HostJson.object
      [("nested", HostJson.Array
        [HostJson.Bool True, HostJson.Null,
         HostJson.object [("long", HostJson.String (HostText.replicate 16384 "x"))]])]
    then pure ()
    else error "nested JSON payload differs from the mounted input"
