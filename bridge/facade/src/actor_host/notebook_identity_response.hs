data CellReply = CellReply Text deriving Show
reviewer <- startAgent (withAgentLifetime ActorOwned (readonlyAgent "nominal-reviewer"))
pending <- do { issued <- request reviewer (assignment [label|nominal-request|] ()); Right () <- detachRequest issued; pure issued }
let pinned = pending :: Response CellReply
pollResponse pinned
let later = pollResponse pinned
later
