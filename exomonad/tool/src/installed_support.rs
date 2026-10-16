//! Interpreter support derived from the installed dispatch stack.

/// Support emitted by an installed interpreter instance, independent of the
/// authored row and the caller's authority. Inert or unavailable instances
/// return no keys even when their static Haskell vocabulary exists.
pub trait InstalledEffectSupport {
    fn installed_effect_support(&self) -> Vec<crate::ToolEffectKey>;

    /// Families this instance recognizes, including inert implementations.
    /// Nominal dispatch stops at the first recognizer in the installed stack.
    fn handled_effect_families(&self) -> Vec<crate::ToolEffectKey> {
        self.installed_effect_support()
    }
}

impl InstalledEffectSupport for frunk::HNil {
    fn installed_effect_support(&self) -> Vec<crate::ToolEffectKey> {
        Vec::new()
    }
}

impl<H: InstalledEffectSupport, T: InstalledEffectSupport> InstalledEffectSupport
    for frunk::HCons<H, T>
{
    fn installed_effect_support(&self) -> Vec<crate::ToolEffectKey> {
        let mut keys = self.head.installed_effect_support();
        let shadowed = self.head.handled_effect_families();
        for key in self.tail.installed_effect_support() {
            if !shadowed.contains(&key) && !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }

    fn handled_effect_families(&self) -> Vec<crate::ToolEffectKey> {
        let mut keys = self.head.handled_effect_families();
        for key in self.tail.handled_effect_families() {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        keys
    }
}

impl<H: InstalledEffectSupport> InstalledEffectSupport for std::sync::Arc<H> {
    fn installed_effect_support(&self) -> Vec<crate::ToolEffectKey> {
        (**self).installed_effect_support()
    }

    fn handled_effect_families(&self) -> Vec<crate::ToolEffectKey> {
        (**self).handled_effect_families()
    }
}

impl<H: InstalledEffectSupport> InstalledEffectSupport for parking_lot::Mutex<H> {
    fn installed_effect_support(&self) -> Vec<crate::ToolEffectKey> {
        self.lock().installed_effect_support()
    }

    fn handled_effect_families(&self) -> Vec<crate::ToolEffectKey> {
        self.lock().handled_effect_families()
    }
}
