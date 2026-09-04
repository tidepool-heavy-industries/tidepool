No deadline is the default. When work must be bounded, use dimensional time;
bare integers are intentionally not accepted.

```haskell
options = withRequestDeadline (after (minutes 10))
        $ requestOptions label task
response <- requestWith worker options
```

`milliseconds`, `seconds`, and `minutes` construct `Duration`; `after`
constructs a `RequestDeadline`. Status preserves the authored unit and shows
absolute and remaining time.
