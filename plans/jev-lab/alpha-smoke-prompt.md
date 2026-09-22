You are working in the Tidepool repository. Load the `exomonad-jev` and
`exomonad-agent-spec` skills before you start.

This checkout has an agent spec, `.exomonad/AgentSpec.hs`, and its tools live in
`.exomonad/Project/Tools.hs`. One of your tools, `triage_search`, is declared but
dumb: it lists every matching file and ignores what you said you were looking
for. Your after-tool slot exists and always abstains. Make both of them smart
with Jev, live, in this session. Everything below is a body edit followed by
`reload_agent_spec`; nothing needs a restart.

1. Start in a cell, not a file. Search for `after_tool` in the Rust sources,
   and ask Jev, per matching file, whether that file is where the after-tool
   slot is invoked, where it is defined, or merely mentions it. Keep the answer
   as a binding and show only the invoked and defined files. Get this right in
   the notebook before you move it anywhere.
2. Move that judgment into `triageSearchBody`, so `triage_search` answers with
   the files that matter for `looking_for` and says briefly why. Reload, then
   call `triage_search` and compare it with what your cell said.
3. Give the after-tool slot a body. When a `bash` or `exec_command` result is
   long enough to hurt, a few hundred lines, read your last few turns, ask Jev
   which passages bear on what you have been doing, and prune to those. Abstain
   when the result is short or the judgment is weak. Reload, run something noisy, and check the pruned view against the
   whole result through its handle.
4. It will be too aggressive or too timid the first time. Adjust it and reload
   until you would want to keep it. Use `status` with the detailed view to see
   what the slot did on each call.
5. Improve the wording of `triage_search`'s description and reload. Report what
   happened and what you would do about it.
6. Commit the two files with a message a later agent could learn from.

Say plainly where a skill or an error message sent you the wrong way. That is
as useful to us as the working slot.
