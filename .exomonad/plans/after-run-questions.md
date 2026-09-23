# After-dogfooding questions

## Field notes

- Which theories tripped, and did a human reading the same trace agree?
  Evidence: notes journal vs. the trace at the pointed handles.
- Which theories never tripped: wrong question, wrong floor, or the behavior
  did not occur? Evidence: the packet's likelihoods over the run.
- Did the root read its own field notes, and did reading them change what it
  did next? Evidence: cells that read the journal; the model's account.
- Was cadence N right? Did anything happen between checks that a note would
  have caught earlier? Evidence: notes timestamps vs. trace.
- What did the hook not see that it needed: the root's own cells, child
  replies, a rejection? Decides whether the hook needs a wider view.

## Rebase router

- On each main advance, what did the router say per child, and was it
  right? Evidence: its journal lines; the child's next rebase or conflict.
- Did a nudge reach the child in its next tool result, or did it need the
  parent? Decides the notification delivery question.
- Did any child rebase without being told, or ignore a nudge? Why?
  Evidence: child transcripts; the model's account.
- Was the exact part (behind, path overlap) enough, and what did Jev add
  in the ambiguous middle? Evidence: the router's likelihoods vs. outcomes.
- Would the root have wanted the router to integrate instead of only
  advise? Ask the root.

## Typed tool results

- When you got a check or command result, what did you do with the text
  before you could act? Ask the root and each child.
- Which cells parse a tool's text output by hand, and into what shapes?
  Evidence: Jev over the trace's cells; one packet.
- Which fields would you have pattern-matched on if the result had been a
  value? Ask the models. Three consistent answers is the type to ship.
- Did the shell presenter's selection ever drop the line you needed? How
  did you recover? Evidence: recovery cells using `outputSnapshot`/`section`.

## Kit as a whole

- Where did you still have to say the obvious next thing?
- Which program let you skip a round without redoing the work later?
- What context did your program need that you already had?
- Which definition would you give the next agent, and did you save it?
- What was the largest rejection class, counted from the trace? Decides
  whether the type-rejection hint parcel is worth building.
- Did any documented feature turn out not to exist or not to work as
  described? List each with the doc that claimed it.

## Delegation

- Did the children stay inside their module? Evidence: numstat per branch.
- How many rounds did review and repair take per child, and what was the
  usual cause? Evidence: the review loop's journal, if any; transcripts.
- Where did a child need context the root had and did not pass?
