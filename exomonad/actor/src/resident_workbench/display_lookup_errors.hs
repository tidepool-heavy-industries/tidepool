import qualified Tidepool.Lookup as LookupApi
import qualified Tidepool.Inspection as Inspection
import qualified Data.Text as Text
let batch = LookupApi.LookupBatch [LookupApi.LookupResult "retained-query" (LookupApi.LookupRejected (Text.replicate 3000 "detail " <> "end-of-retained-query")), LookupApi.LookupResult "second-query" (LookupApi.LookupMissing ["missing-api"] [])] [] "retained-view" Nothing
shown <- Inspection.display batch
