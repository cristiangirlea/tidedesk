//! Bounded text clipboard synchronization. Clipboard contents are never logged.
use crate::protocol::MAX_CLIPBOARD_BYTES;

pub fn valid_text(text: &str) -> bool {
    text.len() <= MAX_CLIPBOARD_BYTES && !text.contains('\0')
}

trait ClipboardIo: Send {
    fn get_text(&mut self) -> Result<String, arboard::Error>;
    fn set_text(&mut self, text: &str) -> Result<(), arboard::Error>;
}

impl ClipboardIo for arboard::Clipboard {
    fn get_text(&mut self) -> Result<String, arboard::Error> {
        arboard::Clipboard::get_text(self)
    }
    fn set_text(&mut self, text: &str) -> Result<(), arboard::Error> {
        arboard::Clipboard::set_text(self, text)
    }
}

/// Keeps clipboard I/O lazy: disabled sessions never read or write the OS clipboard.
#[derive(Default)]
pub struct ClipboardBridge {
    clipboard: Option<Box<dyn ClipboardIo>>,
    enabled: bool,
    baseline: bool,
    last: Option<String>,
}

impl ClipboardBridge {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.baseline = false;
            self.last = None;
            if !enabled {
                self.clipboard = None;
            }
        }
    }

    fn backend(&mut self) -> Option<&mut Box<dyn ClipboardIo>> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new()
                .ok()
                .map(|c| Box::new(c) as Box<dyn ClipboardIo>);
        }
        self.clipboard.as_mut()
    }

    /// Enabling observes a baseline; only subsequent copies are shared.
    pub fn poll(&mut self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let result = self.backend()?.get_text();
        let text = match result {
            Ok(text) if valid_text(&text) => Some(text),
            Ok(_) | Err(arboard::Error::ContentNotAvailable) => None,
            Err(_) => return None, // Busy clipboard: retry at the next tick.
        };
        self.observe(text)
    }

    fn observe(&mut self, text: Option<String>) -> Option<String> {
        if !self.baseline {
            self.baseline = true;
            self.last = text;
            return None;
        }
        if text == self.last {
            return None;
        }
        self.last = text.clone();
        text
    }

    pub fn receive(&mut self, text: &str) -> bool {
        if !self.enabled || !valid_text(text) {
            return false;
        }
        let Some(clipboard) = self.backend() else {
            return false;
        };
        if clipboard.set_text(text).is_err() {
            return false;
        }
        self.last = Some(text.to_owned());
        self.baseline = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeClipboard {
        text: String,
        reads: usize,
        writes: usize,
    }

    impl ClipboardIo for Arc<Mutex<FakeClipboard>> {
        fn get_text(&mut self) -> Result<String, arboard::Error> {
            let mut state = self.lock().unwrap();
            state.reads += 1;
            Ok(state.text.clone())
        }
        fn set_text(&mut self, text: &str) -> Result<(), arboard::Error> {
            let mut state = self.lock().unwrap();
            state.writes += 1;
            state.text = text.into();
            Ok(())
        }
    }

    #[test]
    fn two_clipboards_exchange_new_copies_without_echo_or_disabled_io() {
        let a = Arc::new(Mutex::new(FakeClipboard {
            text: "private a".into(),
            ..Default::default()
        }));
        let b = Arc::new(Mutex::new(FakeClipboard {
            text: "private b".into(),
            ..Default::default()
        }));
        let mut sender = ClipboardBridge {
            clipboard: Some(Box::new(a.clone())),
            ..Default::default()
        };
        let mut receiver = ClipboardBridge {
            clipboard: Some(Box::new(b.clone())),
            ..Default::default()
        };
        assert!(sender.poll().is_none());
        assert!(!receiver.receive("not allowed"));
        assert_eq!(a.lock().unwrap().reads, 0);
        assert_eq!(b.lock().unwrap().writes, 0);
        sender.set_enabled(true);
        receiver.set_enabled(true);
        assert!(sender.poll().is_none());
        assert!(receiver.poll().is_none());
        a.lock().unwrap().text = "fresh copy".into();
        let text = sender.poll().unwrap();
        assert!(receiver.receive(&text));
        assert_eq!(b.lock().unwrap().text, "fresh copy");
        assert!(receiver.poll().is_none());
        b.lock().unwrap().text = "return copy".into();
        assert!(sender.receive(&receiver.poll().unwrap()));
        assert!(sender.poll().is_none());
        receiver.set_enabled(false);
        assert!(!receiver.receive("must not write"));
        assert!(receiver.poll().is_none());
        assert_eq!(b.lock().unwrap().writes, 1);
    }

    #[test]
    fn enable_does_not_upload_existing_text_and_remote_writes_do_not_echo() {
        let mut bridge = ClipboardBridge::default();
        assert!(bridge.poll().is_none());
        assert!(!bridge.receive("disabled"));
        bridge.set_enabled(true);
        assert!(bridge.observe(Some("old secret".into())).is_none());
        assert_eq!(
            bridge.observe(Some("new copy".into())),
            Some("new copy".into())
        );
        assert!(bridge.observe(Some("new copy".into())).is_none());
        // Same state update as a successful receive, without touching the OS in a test.
        bridge.last = Some("remote copy".into());
        assert!(bridge.observe(Some("remote copy".into())).is_none());
        bridge.set_enabled(false);
        bridge.set_enabled(true);
        assert!(bridge.observe(Some("copied while off".into())).is_none());
    }

    #[test]
    fn text_limits_use_utf8_bytes_and_reject_nuls() {
        assert!(valid_text(&"a".repeat(MAX_CLIPBOARD_BYTES)));
        assert!(!valid_text(&"é".repeat(MAX_CLIPBOARD_BYTES)));
        assert!(!valid_text("a\0b"));
        assert!(valid_text(""));
    }
}
