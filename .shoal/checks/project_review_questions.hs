let semantics = Question "semantics" question
let product = Question "product-gate" (question { questionFinding = "Who closes the visible UI gate?" })
let firstQuestions = raiseQuestion semantics []
reportProgress firstQuestions
let openQuestions = raiseQuestion product firstQuestions
reportProgress openQuestions
pollReply sessionReply
