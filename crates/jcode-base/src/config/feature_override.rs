//! Test support for global feature gates.
//!
//! Dormant-feature tests enable a gate through its real environment override
//! inside their own isolated state, then restore it. Production code never
//! constructs this. Callers hold the shared test environment lock.
use std::ffi::OsString;

/// A scoped global feature override through its environment key. The config
/// cache is invalidated on creation and again when the previous value returns.
#[must_use = "the override is removed when dropped"]
pub struct ScopedFeatureOverride {
    key: &'static str,
    previous: Option<OsString>,
}

impl ScopedFeatureOverride {
    /// `features.legacy_work_tracking` through `JCODE_LEGACY_WORK_TRACKING_ENABLED`.
    pub fn legacy_work_tracking(enabled: bool) -> Self {
        Self::set("JCODE_LEGACY_WORK_TRACKING_ENABLED", enabled)
    }

    /// `features.session_work` through `JCODE_SESSION_WORK_ENABLED`.
    pub fn session_work(enabled: bool) -> Self {
        Self::set("JCODE_SESSION_WORK_ENABLED", enabled)
    }

    /// `features.memory` through `JCODE_MEMORY_ENABLED`.
    pub fn memory(enabled: bool) -> Self {
        Self::set("JCODE_MEMORY_ENABLED", enabled)
    }

    fn set(key: &'static str, enabled: bool) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, if enabled { "true" } else { "false" });
        super::invalidate_config_cache();
        Self { key, previous }
    }
}

impl Drop for ScopedFeatureOverride {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var(self.key, value),
            None => crate::env::remove_var(self.key),
        }
        super::invalidate_config_cache();
    }
}
