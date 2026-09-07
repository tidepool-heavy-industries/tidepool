let campaign = either (const (error "invalid fixture campaign")) id (campaignLabel "custody-single")
let wave = either (const (error "invalid fixture wave")) id (forkGroupLabel "worker")
let label = either (const (error "invalid fixture branch")) id (branchLabel "worker")
worker <- unfold (batch campaign wave) (child (coding @Text label projectHead ("custody" :: Text)))
