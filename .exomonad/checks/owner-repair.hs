let Right repairLabel = requestLabel "repair-candidate"
step <- repair repairLabel sessionInput (reviewInput sessionInput) ["Preserve the gate and repair the feature content."]
let Left verdict = step
respond (Produced verdict)
