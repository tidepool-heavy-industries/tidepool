import Tidepool.Agent.Reply (requestId, requestIdNumber)
Right suppliedSecondJob <- request @Int suppliedChild (20 :: Int) (defaultRequestOptions { requestLabel = Just "supplied-live-second-request" })
let suppliedSecondRequestNumber = requestIdNumber (requestId suppliedSecondJob)
display (suppliedSecondRequestNumber /= suppliedFirstRequestNumber)
