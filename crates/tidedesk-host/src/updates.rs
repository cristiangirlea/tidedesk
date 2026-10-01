//! Updates for the build installed from the Microsoft Store.
//!
//! The Store updates its apps on its own, but not while they run, and
//! TideDesk usually runs all the time, in the tray. So the host asks the
//! Store itself whether an update waits, and has the Store install it: on a
//! word from the person at the computer, or on its own, as Settings say.
//! Never while a viewer is connected. Windows ends the program to update it
//! and starts it again afterwards, hidden in the tray.
//!
//! The ZIP build knows no Store and asks nobody.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What to do about updates, from Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Updates {
    /// Install an update as soon as no viewer is connected.
    Automatic,
    /// Do not ask the Store; it updates the app when it does not run.
    Off,
    /// Say that an update waits, and install it when told to. Also what a
    /// word means that this version does not know (for which it is last).
    #[default]
    #[serde(other)]
    Ask,
}

impl Updates {
    pub const ALL: [Self; 3] = [Self::Ask, Self::Automatic, Self::Off];

    /// As Settings name it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask me",
            Self::Automatic => "Install when no one is connected",
            Self::Off => "Leave it to the Store",
        }
    }
}

/// What is to be done about updates at a given moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Nothing,
    /// Ask the Store whether an update waits.
    Look,
    /// Say that one waits, and offer to install it.
    Offer,
    /// Have the Store install it.
    Install,
}

/// What is known about updates, for the window and for the thread that sees
/// to them.
#[derive(Debug, Default)]
pub struct Board {
    pub setting: Updates,
    /// An update waits in the Store.
    pub found: bool,
    /// The Store is installing it.
    pub installing: bool,
    /// Why it could not be installed the last time.
    pub failed: Option<String>,
    /// The title of the program's window, for the Store to ask in front of;
    /// none for a host without one.
    pub window: Option<&'static str>,
    /// "Update now" was chosen.
    wanted: bool,
    /// When "Later" was chosen.
    later: Option<Instant>,
    /// When the Store was last asked.
    looked: Option<Instant>,
}

impl Board {
    /// After the start, before the Store is asked: the computer has more
    /// pressing things to do when it starts.
    const FIRST: Duration = Duration::from_secs(60);
    /// Between two questions to the Store.
    const EVERY: Duration = Duration::from_secs(6 * 60 * 60);
    /// After "Later", before the offer is made again.
    const LATER: Duration = Duration::from_secs(24 * 60 * 60);

    pub fn new(setting: Updates) -> Self {
        Self {
            setting,
            ..Self::default()
        }
    }

    /// What is to be done `now`, in a program `started` then, with a viewer
    /// `connected` or not.
    pub fn step(&self, connected: bool, started: Instant, now: Instant) -> Step {
        let since = |then: Instant| now.saturating_duration_since(then);
        // A word from the person counts whatever Settings say.
        let off = self.setting == Updates::Off && !self.wanted;
        if off || connected || self.installing {
            return Step::Nothing;
        }
        if !self.found {
            let due = match self.looked {
                _ if self.wanted => true,
                Some(looked) => since(looked) >= Self::EVERY,
                None => since(started) >= Self::FIRST,
            };
            return if due { Step::Look } else { Step::Nothing };
        }
        if self.wanted || self.setting == Updates::Automatic {
            Step::Install
        } else if self.later.is_some_and(|later| since(later) < Self::LATER) {
            Step::Nothing
        } else {
            Step::Offer
        }
    }

    /// The Store was asked, and had an update or not.
    pub fn looked(&mut self, found: bool, now: Instant) {
        self.looked = Some(now);
        self.found = found;
        self.failed = None;
        if !found {
            self.wanted = false;
        }
    }

    /// Whether an update waits that could be installed now, whether or not
    /// "Later" was chosen in the window: what the tray's tooltip says.
    pub fn waits(&self, connected: bool) -> bool {
        self.found && !self.installing && !connected && self.setting != Updates::Off
    }

    /// "Update now" was chosen: what waits is installed, after asking the
    /// Store if nothing is known to wait.
    pub fn now(&mut self) {
        self.wanted = true;
        self.failed = None;
    }

    /// "Later" was chosen.
    pub fn later(&mut self, now: Instant) {
        self.wanted = false;
        self.later = Some(now);
    }

    /// The Store begins to install the update.
    pub fn installing(&mut self) {
        self.installing = true;
    }

    /// The Store is done with it. After an update that was installed the
    /// program does not get here: Windows has ended it. The Store is asked
    /// again in a while either way.
    pub fn installed(&mut self, result: Result<(), String>, now: Instant) {
        self.installing = false;
        self.wanted = false;
        self.found = false;
        self.looked = Some(now);
        self.failed = result.err();
    }
}

/// Sees to updates for as long as the program runs, in a thread of its own:
/// asks the Store, and has it install what waits, as the host's board says.
/// Only in the build the Store installed.
pub fn watch(state: std::sync::Arc<crate::session::HostState>) {
    if !crate::platform::is_packaged() {
        return;
    }
    let started = Instant::now();
    let seen = std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            loop {
                // Short, for a word from the person to be followed soon.
                std::thread::sleep(Duration::from_secs(2));
                let connected = state.connected();
                let step = state
                    .updates
                    .lock()
                    .unwrap()
                    .step(connected, started, Instant::now());
                match step {
                    Step::Nothing | Step::Offer => continue,
                    Step::Look => {
                        let found = store::look().unwrap_or_else(|e| {
                            tracing::debug!("the Store did not say whether an update waits: {e:#}");
                            false
                        });
                        if found {
                            tracing::info!("an update waits in the Microsoft Store");
                        }
                        state.updates.lock().unwrap().looked(found, Instant::now());
                    }
                    Step::Install => {
                        let window = {
                            let mut board = state.updates.lock().unwrap();
                            board.installing();
                            board.window
                        };
                        state.changed();
                        tracing::info!("the Microsoft Store installs an update");
                        let result = store::install(window).map_err(|e| format!("{e:#}"));
                        if let Err(e) = &result {
                            tracing::warn!("the update was not installed: {e}");
                        }
                        let mut board = state.updates.lock().unwrap();
                        board.installed(result, Instant::now());
                    }
                }
                state.changed();
            }
        });
    if let Err(e) = seen {
        tracing::warn!("updates are left to the Store: {e}");
    }
}

/// The Microsoft Store, as the package it installed may ask it.
#[cfg(windows)]
mod store {
    use anyhow::{Context, Result, bail};
    use windows::Services::Store::{StoreContext, StorePackageUpdateState};
    use windows::Win32::System::Recovery::{
        REGISTER_APPLICATION_RESTART_FLAGS, RegisterApplicationRestart,
    };
    use windows::Win32::UI::Shell::IInitializeWithWindow;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::{HSTRING, Interface};

    /// Whether an update waits.
    pub fn look() -> Result<bool> {
        let store = StoreContext::GetDefault()?;
        let updates = store.GetAppAndOptionalStorePackageUpdatesAsync()?.join()?;
        Ok(updates.Size()? > 0)
    }

    /// Has the Store install what waits: without a word where the person's
    /// Store settings allow it, else with the Store's own question, in front
    /// of the window titled `window`.
    pub fn install(window: Option<&str>) -> Result<()> {
        let store = StoreContext::GetDefault()?;
        let updates = store.GetAppAndOptionalStorePackageUpdatesAsync()?.join()?;
        if updates.Size()? == 0 {
            return Ok(());
        }
        // Windows ends the program to update it. It starts it again if
        // asked to, here as at sign-in: hidden in the tray.
        let again = HSTRING::from(super::restart_arguments(crate::self_prefix()));
        unsafe { RegisterApplicationRestart(&again, REGISTER_APPLICATION_RESTART_FLAGS(0)) }
            .context("Windows would not start TideDesk again after the update")?;
        let result = if store.CanSilentlyDownloadStorePackageUpdates()? {
            store
                .TrySilentDownloadAndInstallStorePackageUpdatesAsync(&updates)?
                .join()?
        } else {
            let window = window.context("the Store wants to ask, and there is no window")?;
            // In front of the window, which is hidden while in the tray.
            crate::platform::set_window_visible(window, true);
            let owner = unsafe { FindWindowW(None, &HSTRING::from(window)) }
                .context("the Store wants to ask, and the window is not to be found")?;
            unsafe { store.cast::<IInitializeWithWindow>()?.Initialize(owner) }?;
            store
                .RequestDownloadAndInstallStorePackageUpdatesAsync(&updates)?
                .join()?
        };
        match result.OverallState()? {
            StorePackageUpdateState::Completed => Ok(()),
            state => bail!("the Store stopped at step {}", state.0),
        }
    }
}

#[cfg(not(windows))]
mod store {
    pub fn look() -> anyhow::Result<bool> {
        Ok(false)
    }

    pub fn install(_: Option<&str>) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The command line, after the program's name, that starts the host again
/// after an update: hidden in the tray.
#[cfg_attr(not(windows), allow(dead_code))]
fn restart_arguments(prefix: &[&str]) -> String {
    prefix
        .iter()
        .copied()
        .chain(["--tray"])
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);
    const HOUR: Duration = Duration::from_secs(60 * 60);

    /// A board on which the Store was found to have an update a minute after
    /// the start, with `setting` chosen since.
    fn found(setting: Updates) -> (Board, Instant) {
        let started = Instant::now();
        let mut board = Board::new(Updates::Ask);
        assert_eq!(board.step(false, started, started + MINUTE), Step::Look);
        board.looked(true, started + MINUTE);
        board.setting = setting;
        (board, started)
    }

    #[test]
    fn the_store_is_asked_a_minute_after_the_start_and_every_six_hours() {
        let started = Instant::now();
        let mut board = Board::new(Updates::Ask);
        let step = |board: &Board, after| board.step(false, started, started + after);
        assert_eq!(step(&board, Duration::ZERO), Step::Nothing);
        assert_eq!(step(&board, MINUTE - Duration::from_secs(1)), Step::Nothing);
        assert_eq!(step(&board, MINUTE), Step::Look);
        board.looked(false, started + MINUTE);
        assert_eq!(step(&board, 2 * MINUTE), Step::Nothing);
        assert_eq!(step(&board, 6 * HOUR), Step::Nothing);
        assert_eq!(step(&board, 6 * HOUR + MINUTE), Step::Look);
    }

    #[test]
    fn an_update_is_offered_and_installed_on_a_word() {
        let (mut board, started) = found(Updates::Ask);
        let at = started + 2 * MINUTE;
        assert_eq!(board.step(false, started, at), Step::Offer);
        assert_eq!(board.step(false, started, at + 30 * HOUR), Step::Offer);
        board.now();
        assert_eq!(board.step(false, started, at), Step::Install);
        board.installing();
        assert_eq!(board.step(false, started, at), Step::Nothing);
    }

    #[test]
    fn later_means_a_day_later() {
        let (mut board, started) = found(Updates::Ask);
        let at = started + 2 * MINUTE;
        board.later(at);
        assert_eq!(board.step(false, started, at), Step::Nothing);
        assert_eq!(board.step(false, started, at + 23 * HOUR), Step::Nothing);
        assert_eq!(board.step(false, started, at + 24 * HOUR), Step::Offer);
        // Nor is the Store asked again meanwhile: it is known what waits.
        assert!(board.found);
    }

    #[test]
    fn an_update_is_installed_without_a_word_where_settings_say_so() {
        let (board, started) = found(Updates::Automatic);
        assert_eq!(
            board.step(false, started, started + 2 * MINUTE),
            Step::Install
        );
    }

    #[test]
    fn nothing_is_done_while_a_viewer_is_connected() {
        let started = Instant::now();
        let at = started + 2 * MINUTE;
        assert_eq!(
            Board::new(Updates::Ask).step(true, started, at),
            Step::Nothing
        );
        for setting in Updates::ALL {
            let (mut board, started) = found(setting);
            board.now();
            assert_eq!(board.step(true, started, at), Step::Nothing, "{setting:?}");
        }
        // Once the viewer has gone, what was chosen is done.
        let (mut board, started) = found(Updates::Ask);
        board.now();
        assert_eq!(board.step(false, started, at), Step::Install);
    }

    #[test]
    fn off_leaves_it_to_the_store() {
        let (board, started) = found(Updates::Off);
        for after in [Duration::ZERO, MINUTE, 7 * HOUR, 48 * HOUR] {
            assert_eq!(board.step(false, started, started + after), Step::Nothing);
        }
        assert_eq!(
            Board::new(Updates::Off).step(false, started, started + 7 * HOUR),
            Step::Nothing
        );
    }

    #[test]
    fn an_update_that_failed_is_tried_again_after_hours_not_at_once() {
        let (mut board, started) = found(Updates::Automatic);
        let at = started + 2 * MINUTE;
        board.installing();
        board.installed(Err("the Store stopped at step 4".into()), at);
        assert_eq!(board.failed.as_deref(), Some("the Store stopped at step 4"));
        assert_eq!(board.step(false, started, at + MINUTE), Step::Nothing);
        assert_eq!(board.step(false, started, at + 6 * HOUR), Step::Look);
        board.looked(true, at + 6 * HOUR);
        assert_eq!(board.failed, None);
        assert_eq!(board.step(false, started, at + 6 * HOUR), Step::Install);
    }

    #[test]
    fn a_failure_is_forgotten_when_the_store_is_asked_again() {
        let (mut board, started) = found(Updates::Ask);
        let at = started + 2 * MINUTE;
        board.installed(Err("the Store stopped at step 4".into()), at);
        // The Store updated the app itself meanwhile, or withdrew the update.
        board.looked(false, at + 6 * HOUR);
        assert_eq!(board.failed, None);
    }

    /// What the tray's tooltip says: whatever was chosen in the window.
    #[test]
    fn the_tray_says_what_waits() {
        let (mut board, started) = found(Updates::Ask);
        let at = started + 2 * MINUTE;
        assert!(board.waits(false));
        board.later(at);
        assert!(board.waits(false));
        assert!(!board.waits(true), "not with a viewer connected");
        board.installing();
        assert!(!board.waits(false));
        assert!(!found(Updates::Off).0.waits(false));
        assert!(!Board::new(Updates::Ask).waits(false), "nothing found");
    }

    /// "Update TideDesk now" in the tray's menu, which is there whether or
    /// not an update is known to wait: the Store is asked at once, and what
    /// it has is installed.
    #[test]
    fn a_word_is_enough_to_ask_the_store_and_install() {
        for setting in Updates::ALL {
            let started = Instant::now();
            let mut board = Board::new(setting);
            board.now();
            // Not in the first minute's turn, nor in six hours.
            assert_eq!(board.step(false, started, started), Step::Look);
            assert_eq!(board.step(true, started, started), Step::Nothing);
            board.looked(true, started);
            assert_eq!(board.step(false, started, started), Step::Install);

            // With nothing in the Store, that is that.
            let mut board = Board::new(setting);
            board.looked(false, started);
            board.now();
            assert_eq!(board.step(false, started, started), Step::Look);
            board.looked(false, started);
            assert_eq!(board.step(false, started, started), Step::Nothing);
        }
    }

    /// A word that this version does not know, from a hand or from a later
    /// version, must not cost the other settings.
    #[test]
    fn an_unknown_word_means_ask() {
        #[derive(Deserialize)]
        struct File {
            updates: Updates,
        }
        let file: File = toml::from_str("updates = \"nightly\"").unwrap();
        assert_eq!(file.updates, Updates::Ask);
    }

    #[test]
    fn settings_are_saved_in_words() {
        #[derive(Serialize, Deserialize)]
        struct File {
            updates: Updates,
        }
        for (setting, word) in [
            (Updates::Ask, "ask"),
            (Updates::Automatic, "automatic"),
            (Updates::Off, "off"),
        ] {
            let text = toml::to_string(&File { updates: setting }).unwrap();
            assert_eq!(text.trim(), format!("updates = \"{word}\""));
            assert_eq!(toml::from_str::<File>(&text).unwrap().updates, setting);
        }
    }

    #[test]
    fn the_host_starts_again_in_the_tray() {
        assert_eq!(restart_arguments(&["host"]), "host --tray");
        assert_eq!(restart_arguments(&[]), "--tray");
    }
}
