# exomonad-tool

Owns transport-neutral model tool declarations (`HostedTool` and `ToolDeclaration`) and comparison of active and candidate surfaces through `surface::compare_surfaces`. A spec reload must be refused when the declared surface changes; compare only, then swap implementations when the declarations match. Focused check: `cargo test -p exomonad-tool`.
