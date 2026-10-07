observed <- await (observed inheritedWatch)
let retained = case observed of
      Right (Right answer) -> answer
      _ -> error "inherited request did not settle"
inspectFull (fst retained, snd retained 41)
