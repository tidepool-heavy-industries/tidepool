readContext :: Member ContextReadWrite effects => Eff effects ()
readContext = Core.getContext >> pure ()

-- Authors can still name an explicit row with an ordinary lexical alias.
type M = Eff '[ContextReadWrite]
explicitAlias :: M ()
explicitAlias = readContext
