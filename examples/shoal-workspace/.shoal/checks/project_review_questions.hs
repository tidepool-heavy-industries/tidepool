let semantics = Question "semantics" question
let product = Question "product-gate" (question { questionFinding = "Who closes the visible UI gate?" })
let firstQuestions = raiseQuestion semantics []
reportProgress (WorkProgress [] firstQuestions)
let openQuestions = raiseQuestion product firstQuestions
reportProgress (WorkProgress [] openQuestions)
import Tidepool.Agent.Reply (pollReply)
pollReply sessionReply
