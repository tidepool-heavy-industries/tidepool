import qualified Tidepool.Lookup.Tools as LookupTools
topics <- LookupApi.lookupRaw (LookupApi.lookupRequest ["doc topics"])
display (LookupTools.renderBatch topics)
