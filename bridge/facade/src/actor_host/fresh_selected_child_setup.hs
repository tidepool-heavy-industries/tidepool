freshSelectedChild <- unfold (batch "fresh-selected" "original-owner") $
  child (withContext (selected (\(FreshParentInput n) -> T.pack (show n)))
    (withLifetime ActorOwned (narrowed @'[Replies, Console] @FreshParentReply knownEffects
      (inspectionPolicy currentCheckout)
      (assignment [label|fresh-selected-child|] (FreshParentInput 41)))))
