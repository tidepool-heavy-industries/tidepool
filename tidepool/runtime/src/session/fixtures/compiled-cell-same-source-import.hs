import qualified SameCellImportSupport as First
import qualified SameCellImportSupport as Second

sameCellOriginal = First.answerValue + Second.answerValue

if sameCellOriginal == 42 then pure () else error "same-cell quoted original returned the wrong value"
