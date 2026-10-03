{-# LANGUAGE NoImplicitPrelude #-}
{{CELL_PRAGMAS}}
module CellCheck where
import Prelude
{{CELL_IMPORTS}}
__tidepoolCellExpression :: value -> IO ()
__tidepoolCellExpression _ = pure ()
__tidepoolInEffectRow :: IO value -> IO value
__tidepoolInEffectRow = id
__tidepoolCellDisplayConstraint :: Show value => value -> ()
__tidepoolCellDisplayConstraint _ = ()
{{CELL_DECLS}}
__cell :: IO ()
__cell = do {
{{CELL_BODY}}
; pure () }
