let nextStep = J.choice "Which next step?" (J.alt #profile "Time it" ["true"] J..| J.alt #nothing "Nothing" ["false"])
picked <- J.ask (J.state (#task := ("t" :: Text))) (#next := nextStep)
fmap (\a -> J.handle a.next (#profile id J..| #nothing id)) picked
