data M2Input = M2Input Int deriving Show
data M2Reply = M2Reply Int deriving Show
let m2MakeReply (M2Input value) = M2Reply (value + 1)
let m2OriginalInput = M2Input 42
display True
