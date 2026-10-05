data Task = Task Int (Eff '[Replies] Int)

projectTask :: Task -> Int
projectTask (Task value _) = value

actionTask :: Task -> Eff '[Replies] Int
actionTask (Task _ action) = action

let originalProject = projectTask :: Task -> Int
let originalAction = actionTask :: Task -> Eff '[Replies] Int
let originalInput = Task 42 (pure 43)
