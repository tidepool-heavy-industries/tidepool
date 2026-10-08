next <- repair "repair-candidate" sessionInput (reviewInput sessionInput) ["preserve the product gate"]
let Right (Right revision) = next
