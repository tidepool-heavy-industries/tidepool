import Tidepool.Agent.Reply (requestId, requestIdNumber)
Right suppliedJob <- request @Int suppliedChild (10 :: Int) (defaultRequestOptions { requestLabel = Just "supplied-live-request" })
let suppliedFirstRequestNumber = requestIdNumber (requestId suppliedJob)
display True
