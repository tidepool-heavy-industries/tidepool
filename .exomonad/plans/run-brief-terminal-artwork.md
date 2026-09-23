# Terminal artwork run brief

Build a small standalone terminal artwork: a mathematical performance in
discrete movements, each exploring a different precise system as it becomes
unstable. Combine the visual language of scientific instruments, geometric
studies, and analog video synthesis. Think fine trajectories, phase portraits,
cross-sections, and interference fields. The result should feel deliberately
composed, with a beginning, development, and ending.

Use sparse, sharply defined foreground geometry against saturated backgrounds.
Within each movement, the geometry and field should express the same underlying
dynamics: traces might sample a field, expose its structure, or reveal where
nearby trajectories diverge. Favor a restrained palette with strong color
relationships. Darkness and empty space should have compositional weight.
Terminal constraints, character cells, limited resolution, and color, are part
of the medium.

Make the pacing propulsive, with contemplative negative space. Build pressure
through acceleration, interference, symmetry breaking, or parameter changes,
then hold or clear the image long enough for the viewer to perceive what
happened. Give each movement a recognizable identity and compose intentional
transitions between them. Choose three mathematical systems that offer
distinct behaviors and suit the medium; use your judgment about equations,
rendering techniques, and colors.

Keep the project small and quick to build and test. Aim for a complete first
performance early, then refine it. Provide a simple launch command, a reliable
quit action, and clean terminal restoration. Deterministic seeds or time-based
rendering should make interesting moments reproducible. Keep dependencies and
configuration modest; add controls only when they improve the experience.

Structure the work so it divides cleanly. You are the Sol lead. First, in your
own checkout, write the shared rendering contract: the cell buffer and color
model, a movement interface with a fixed signature (seed, time, size in cells)
that returns a frame, and a sequencer that plays movements with transitions.
Commit it to main before delegating; children are made from a commit. Then
unfold three Luna children, one per movement, each in its own worktree, each
owning exactly one module under a movements directory and nothing else. Give
each the contract, its system, and a two-line aesthetic direction, and let it
make the mathematical and visual decisions. While they work, keep improving the
sequencer and transitions on main yourself, so main advances under them at
least twice; that is deliberate. Children commit small and often to their own
branch. When a child reports done, review its candidate commit, integrate it
through the integration worktree, and ask for a repair rather than fixing it
yourself. Own the overall aesthetic, sequencing, integration, and final review.

This is also a dogfooding run for Exomonad. Use the tools and instructions
supplied by your workspace, including Jev-assisted capabilities wherever they
naturally help. Field notes and automatic rebase advice are being prepared
separately; this run does not depend on them. I will observe, converse with you, and ask questions about
decisions and progress. Keep explanations grounded in what actually happened,
and retain enough evidence to discuss useful selections, missed context,
delegation friction, and recovery afterward.

Your working environment is this project and its `.exomonad/` directory. If the
hosting tools fail, preserve the exact operation and error and tell me what is
blocked; I can investigate the host separately. Deliver the artwork with
concise running instructions and an honest account of what you tested and what
still needs visual judgment.
