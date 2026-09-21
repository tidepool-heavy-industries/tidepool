//! Owned Haskell compile inputs for a test's declared effect surface.

use std::io;
use std::path::{Path, PathBuf};

use tidepool_mcp::{CompanionImports, EffectDecl};

/// Optional authored vocabulary layered on top of an effect row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestEffectSurfaceOptions {
    pub companion_imports: CompanionImports,
    pub user_library: bool,
}

impl Default for TestEffectSurfaceOptions {
    fn default() -> Self {
        Self {
            companion_imports: CompanionImports::Omit,
            user_library: false,
        }
    }
}

/// Complete, owned compile inputs derived from one explicit effect row.
#[derive(Debug, Clone)]
pub struct TestEffectSurface {
    declarations: Vec<EffectDecl>,
    include_paths: Vec<PathBuf>,
    preamble: String,
    row: String,
}

impl TestEffectSurface {
    /// Build the smallest surface for `declarations`: no companion imports or
    /// repository-local user library unless requested through [`with_options`].
    pub fn minimal(declarations: &[EffectDecl]) -> io::Result<Self> {
        Self::with_options(declarations, TestEffectSurfaceOptions::default())
    }

    pub fn with_options(
        declarations: &[EffectDecl],
        options: TestEffectSurfaceOptions,
    ) -> io::Result<Self> {
        let dirs = tidepool_mcp::ensure_effects_module(declarations)?;
        let mut include_paths = vec![super::eval_harness::prelude_path()];
        if options.user_library {
            include_paths.push(super::eval_harness::user_lib_dir());
        }
        include_paths.extend(dirs.include_paths());
        Ok(Self {
            declarations: declarations.to_vec(),
            include_paths,
            preamble: tidepool_mcp::build_preamble_with_companions(
                declarations,
                options.user_library,
                options.companion_imports,
            ),
            row: tidepool_mcp::build_effect_stack_type(declarations),
        })
    }

    #[must_use]
    pub fn declarations(&self) -> &[EffectDecl] {
        &self.declarations
    }

    #[must_use]
    pub fn include_paths(&self) -> &[PathBuf] {
        &self.include_paths
    }

    #[must_use]
    pub fn preamble(&self) -> &str {
        &self.preamble
    }

    #[must_use]
    pub fn row(&self) -> &str {
        &self.row
    }

    #[must_use]
    pub fn include_path_refs(&self) -> Vec<&Path> {
        self.include_paths.iter().map(PathBuf::as_path).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_surface_owns_only_required_inputs() {
        let decls = [tidepool_mcp::actor_decl()];
        let surface = TestEffectSurface::minimal(&decls).expect("minimal surface");

        assert_eq!(surface.declarations().len(), 1);
        assert_eq!(surface.declarations()[0].type_name, "Actor");
        assert_eq!(surface.row(), "'[Actor]");
        assert_eq!(surface.include_paths().len(), 3);
        assert!(!surface.preamble().contains("Tidepool.User"));

        let expanded = TestEffectSurface::with_options(
            &decls,
            TestEffectSurfaceOptions {
                companion_imports: CompanionImports::Include,
                user_library: true,
            },
        )
        .expect("expanded surface");
        assert_eq!(expanded.include_paths().len(), 4);
        assert_ne!(surface.preamble(), expanded.preamble());
    }
}
