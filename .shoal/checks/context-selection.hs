import Project.ContextSelection (numberedChunks)
let contextSelectionChunks = numberedChunks 2 "alpha\nbeta\ngamma\ndelta\nepsilon"
check "context selection preserves one-based line ranges" (map (\(start, end, _) -> (start, end)) contextSelectionChunks == [(1, 2), (3, 4), (5, 5)])
check "context selection prefixes retained lines" (case contextSelectionChunks of { ((_, _, first) : _) -> first == "1: alpha\n2: beta\n"; _ -> False })
check "an invalid chunk size produces no selection candidates" (null (numberedChunks 0 "alpha"))
