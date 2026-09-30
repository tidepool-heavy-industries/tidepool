{-# LANGUAGE GADTs, TypeFamilies, FlexibleInstances, StandaloneDeriving, DeriveGeneric, UndecidableInstances #-}
{{CELL_PRAGMAS}}
module PlannedCheck where
import Prelude
import DuplicateFields (One(same))
{{CELL_IMPORTS}}
__tidepoolCellExpression :: a -> IO ()
__tidepoolCellExpression _ = pure ()
{{CELL_DECLS}}
__result :: IO ()
__result = do { {{CELL_BODY}} }
