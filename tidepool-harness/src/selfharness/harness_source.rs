//! WS-D seam: bootstrap-load for the authored harness source — `render`,
//! `loop`, and the concrete `State` declaration (`examples/harness/Harness.hs`
//! is the reference contract this loads). Loaded ONCE at driver bootstrap,
//! NEVER per-turn (02-runtime.md: "Runtime-invoked at loop boundaries
//! only... The harness author cannot call `render` mid-loop" — the
//! anti-pattern this seam exists to make unrepresentable is a per-turn
//! reload that would blow the cache prefix). Mirrors the decl-plane load
//! pattern in `tidepool_runtime::session::SessionLib`/`ResidentSession::
//! define_scoped`, applied once at boot instead of per-turn.

use std::path::Path;

/// The authored harness's Haskell source, split into the three declarations
/// the driver loads once at bootstrap: `render :: State -> Maybe Text ->
/// Text`, `loop :: State -> Harness State`, and the concrete `State` type
/// (deriving `ToJSON`/`FromJSON`, per 02-runtime.md's `State` contract).
/// Kept as raw source text here — WS-D's bootstrap-load stub compiles it
/// onto the driver's decl plane (a `define_scoped`-shaped call) rather than
/// this module owning compilation.
#[derive(Debug, Clone)]
pub struct HarnessSource {
    pub render_src: String,
    pub loop_src: String,
    pub state_src: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessSourceError {
    #[error("failed to read harness source at {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("harness source at {path} is missing a `{decl}` declaration")]
    MissingDecl { path: String, decl: &'static str },
}

/// Load `render`/`loop`/`State` from a harness source file (e.g.
/// `examples/harness/Harness.hs`) at bootstrap, splitting it into the three
/// declarations [`HarnessSource`] carries. WS-D.
pub fn load_harness_source(path: &Path) -> Result<HarnessSource, HarnessSourceError> {
    let _ = path;
    unimplemented!(
        "WS-D: parse render/loop/State out of the harness source file at bootstrap, \
         once — never per-turn"
    )
}
