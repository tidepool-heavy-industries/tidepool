//! Runtime filesystem access retained by the worktree registry.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use tidepool_node::MountNamespace;

#[derive(Clone, Debug)]
pub(crate) struct MountedView {
    pub namespace: MountNamespace,
    pub root: PathBuf,
}

#[derive(Clone, Debug)]
enum ViewAccess {
    Unavailable,
    Mounted(MountedView),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorktreeViews(Arc<RwLock<BTreeMap<PathBuf, ViewAccess>>>);

impl WorktreeViews {
    pub fn require(&self, cwd: &Path) -> io::Result<()> {
        self.0
            .write()
            .map_err(|_| io::Error::other("worktree view lock poisoned"))?
            .entry(cwd.to_owned())
            .or_insert(ViewAccess::Unavailable);
        Ok(())
    }

    pub fn install(&self, cwd: &Path, view: MountedView) -> io::Result<()> {
        self.0
            .write()
            .map_err(|_| io::Error::other("worktree view lock poisoned"))?
            .insert(cwd.to_owned(), ViewAccess::Mounted(view));
        Ok(())
    }

    /// Translate registered identity paths at the filesystem owner. Callers and
    /// durable receipts continue to use the unique registered checkout path.
    pub fn resolve(&self, path: &Path) -> io::Result<Option<MountedView>> {
        let views = self
            .0
            .read()
            .map_err(|_| io::Error::other("worktree view lock poisoned"))?;
        for ancestor in path.ancestors() {
            if let Some(access) = views.get(ancestor) {
                return match access {
                    ViewAccess::Unavailable => Err(io::Error::other(format!(
                        "mounted worktree {} requires filesystem recovery",
                        ancestor.display()
                    ))),
                    ViewAccess::Mounted(view) => Ok(Some(MountedView {
                        namespace: view.namespace.clone(),
                        root: view
                            .root
                            .join(path.strip_prefix(ancestor).map_err(io::Error::other)?),
                    })),
                };
            }
        }
        Ok(None)
    }
}
