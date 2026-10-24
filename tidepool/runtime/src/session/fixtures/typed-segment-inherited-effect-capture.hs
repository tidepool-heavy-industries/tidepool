inheritedEffectValue <- segmentRecord (11 :: Int) >> pure (23 :: Int)
{-# NOINLINE inheritedEffectAction #-}
inheritedEffectAction :: StageEff.Eff __EFFECT_ROW__ ()
inheritedEffectAction = segmentRecord inheritedEffectValue
{-# NOINLINE unreachedEffectAction #-}
unreachedEffectAction :: StageEff.Eff __EFFECT_ROW__ ()
unreachedEffectAction = segmentRecord (997 :: Int)
inheritedEffectAction
