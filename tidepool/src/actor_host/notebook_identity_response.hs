data CellReply = CellReply Text deriving Show
reviewer <- startAgent (readonlyAgent "nominal-reviewer")
pending <- request reviewer (assignment "nominal-request" ())
let pinned = pending :: Response CellReply
pollResponse pinned
let later = pollResponse pinned
later
