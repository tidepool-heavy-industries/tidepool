module QuoteContexts where

import qualified Quote as Q

expression = ([firstQ|literal body with [fakeQuote| text|],
              [againQ|one|], [againQ|two|], [Q.qualifiedQ|three|])
patternUse [patternQ|pattern|] = ()
type TypeUse = [typeQ|type|]
[declarationQ|declaration|]
quotation = [| [nestedQ|nested|] |]
splice = $([| [spliceQ|inside splice|] |])
