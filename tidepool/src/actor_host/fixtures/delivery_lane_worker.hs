let Right campaign = campaignLabel "feature-delivery"
let Right implementationWave = forkGroupLabel "implementation"
let Right implementer = branchLabel "feature"
let Right reviewWave = forkGroupLabel "review"
let Right reviewer = branchLabel "feature-review"
let Right integrationWave = forkGroupLabel "integration"
let Right integrator = branchLabel "feature-integrate"
let lane = DeliveryLane sessionInput (batch campaign implementationWave) implementer boundHead (batch campaign reviewWave) reviewer (batch campaign integrationWave) integrator boundHead
flow <- deliverLane lane sessionReply
