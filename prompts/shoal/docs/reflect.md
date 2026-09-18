# Read your own recent conversation

`reflect n` returns your own last `n` completed turns, oldest first. Each turn
carries what happened inside it in order: the messages, the tool calls, and the
tool results. A call and its result are separate items sharing one call
identity, so you can rejoin the pair.

```haskell signatures
reflect :: Member Reflect effects => Int -> Eff effects (Either ReflectError [ConversationTurn])
data ConversationTurn = ConversationTurn
  { turnIdentity :: Text, turnStartedAt :: Maybe Text
  , turnCompletedAt :: Maybe Text, turnItems :: [TurnItem] }
data TurnItem
  = TurnMessage ConversationRole Text
  | TurnToolCall Text Text Text
  | TurnToolResult Text Text
data ReflectError = ReflectUnbound | ReflectUnreadable Text
```

It reads only your own conversation. There is no argument naming another actor
or a file. `Left ReflectUnbound` means this context has no conversation of its
own — an operator proxy is one — and no other conversation is returned in its
place. The turn you are executing has not completed and is never included.
Fewer than `n` completed turns returns the ones that exist; `n <= 0` returns
none.

Because it is an ordinary effect, a question you ask later — a Jev evaluation
among them — gets your recent context without you spending a turn restating it.
Fetch once, then reuse the value across a whole packet. It is free in your
effort and in extra model turns; the history still counts as input tokens
wherever you send it.

```haskell
-- Your own recent turns, or none when this context has no conversation.
recentContext :: Member Reflect effects => Int -> Eff effects [ConversationTurn]
recentContext n = do
  seen <- reflect n
  case seen of
    Right turns -> pure turns
    -- Continue without history rather than borrowing someone else's.
    Left _ -> pure []

-- The instructions you were given and the tool output you already paid for.
instructionsAndResults :: [ConversationTurn] -> [Text]
instructionsAndResults turns =
  [ text
  | turn <- turns
  , item <- turnItems turn
  , text <- case item of
      TurnMessage RoleUser instruction -> [instruction]
      TurnToolResult _ output -> [output]
      _ -> []
  ]

background <- instructionsAndResults <$> recentContext 5
background
```

`background` is now the context argument for whatever decides next, and the
same value serves every later call in the packet. An authored function can call
`recentContext` itself, combine the result with fresh artifacts, ask its
question, and act — with its caller supplying only the task-specific arguments.

Select before sending. The turns hold everything, including long tool output;
take the items the question actually needs.
