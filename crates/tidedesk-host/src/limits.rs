//! What one viewer may do here, as a program built on TideDesk decides it
//! (an organisation's rights per technician, say), on top of this host's
//! own permissions: a limit can only take away. Looked up once, when the
//! viewer is let in.

use std::sync::{Arc, RwLock};

/// What a viewer may do. Everything, unless a program says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Mouse and keyboard.
    pub input: bool,
    pub clipboard: bool,
    /// Sending files to this computer.
    pub files: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            input: true,
            clipboard: true,
            files: true,
        }
    }
}

/// Decides a viewer's limits, by its certificate's fingerprint (normalised;
/// `None` for a viewer that showed no certificate).
pub trait LimitSource: Send + Sync {
    fn limits(&self, fingerprint: Option<&str>) -> Limits;
}

static SOURCE: RwLock<Option<Arc<dyn LimitSource>>> = RwLock::new(None);

/// Sets the program's source of limits, replacing any earlier one.
pub fn set_source(source: Arc<dyn LimitSource>) {
    *SOURCE.write().unwrap() = Some(source);
}

/// A viewer's limits now.
pub fn for_viewer(fingerprint: Option<&str>) -> Limits {
    let normal = fingerprint.map(tidedesk_core::identity::normalize_fingerprint);
    SOURCE
        .read()
        .unwrap()
        .as_ref()
        .map_or_else(Limits::default, |s| s.limits(normal.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lets one fingerprint only look; everyone else everything.
    struct LookOnly(String);

    impl LimitSource for LookOnly {
        fn limits(&self, fingerprint: Option<&str>) -> Limits {
            if fingerprint == Some(self.0.as_str()) {
                Limits {
                    input: false,
                    clipboard: false,
                    files: false,
                }
            } else {
                Limits::default()
            }
        }
    }

    #[test]
    fn a_program_may_limit_one_viewer() {
        set_source(Arc::new(LookOnly("AAAA1111".into())));
        assert!(!for_viewer(Some("aaaa 1111")).input, "any spelling");
        assert!(!for_viewer(Some("AAAA 1111")).files);
        assert_eq!(for_viewer(Some("BBBB 2222")), Limits::default());
        assert_eq!(for_viewer(None), Limits::default());
    }
}
