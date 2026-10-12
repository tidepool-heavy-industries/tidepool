currentStartupPublication <- C.getInitialContext
_ <- if currentStartupPublication == startupPublication then pure () else error "historical startup evidence changed after edit"
startupCurrent <- C.getContext
let startupMatches = [body | C.Text (Just reference) _ body _ <- view C.contextBlocks startupCurrent, reference == startupReference]
_ <- if startupMatches == ["startup-refreshed"] then pure () else error "refresh did not preserve the issued reference"
C.putContext (over C.contextBlocks (map (startupReplace "startup-refreshed-again")) startupCurrent)
display True
