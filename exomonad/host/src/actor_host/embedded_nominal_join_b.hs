data M2Input = M2Input Int deriving Show
data M2Reply = M2Reply Int deriving Show
data M2JoinB = M2JoinB Int deriving Show
let m2MakeReply (M2Input value) = M2Reply (value + 100)
m2JoinB <- pure (M2JoinB 99)
m2Shadow <- pure (M2Input 99)
display True
