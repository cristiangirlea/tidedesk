//! Saved viewer controls and editable session shortcuts.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use winit::keyboard::{ModifiersState, PhysicalKey};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl Shortcut {
    fn default_for(key: &str) -> Self {
        Self {
            ctrl: true,
            alt: true,
            shift: false,
            key: key.into(),
        }
    }

    pub fn matches(&self, modifiers: ModifiersState, key: PhysicalKey) -> bool {
        let PhysicalKey::Code(key) = key else {
            return false;
        };
        !modifiers.super_key()
            && modifiers.control_key() == self.ctrl
            && modifiers.alt_key() == self.alt
            && modifiers.shift_key() == self.shift
            && format!("{key:?}") == self.key
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        parts.push(self.key.strip_prefix("Key").unwrap_or(&self.key));
        parts.join("+")
    }

    fn valid(&self) -> bool {
        (self.ctrl || self.alt) && keys().contains(&self.key)
    }
}

fn keys() -> Vec<String> {
    ('A'..='Z')
        .map(|c| format!("Key{c}"))
        .chain((1..=12).map(|n| format!("F{n}")))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSettings {
    pub clipboard: bool,
    pub mouse: bool,
    /// Save files the host sends, in Downloads\TideDesk.
    pub allow_files: bool,
    pub game_boost: bool,
    pub clipboard_shortcut: Shortcut,
    pub mouse_shortcut: Shortcut,
    pub game_boost_shortcut: Shortcut,
    pub settings_shortcut: Shortcut,
    pub chat_shortcut: Shortcut,
    /// `host[:port]` of the rendezvous service used to connect by device ID;
    /// empty means TideDesk's own.
    pub rendezvous_server: String,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            clipboard: false,
            mouse: true,
            allow_files: true,
            game_boost: false,
            clipboard_shortcut: Shortcut::default_for("KeyC"),
            mouse_shortcut: Shortcut::default_for("KeyM"),
            game_boost_shortcut: Shortcut::default_for("KeyG"),
            settings_shortcut: Shortcut::default_for("KeyS"),
            chat_shortcut: Shortcut::default_for("KeyT"),
            rendezvous_server: String::new(),
        }
    }
}

impl ViewerSettings {
    pub fn path() -> Result<PathBuf> {
        Ok(tidedesk_core::paths::config_dir()?.join("viewer.toml"))
    }

    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path()?)
    }

    fn load_from(path: &std::path::Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Self::parse(&std::fs::read_to_string(path)?)
    }

    fn parse(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text)?;
        // Older users may already use Ctrl+Alt+G for mouse or clipboard.
        // Preserve those bindings instead of resetting their whole configuration.
        let document: toml::Value = toml::from_str(text)?;
        if document.get("game_boost_shortcut").is_none() {
            config.game_boost_shortcut = ["KeyG", "KeyB", "F9"]
                .into_iter()
                .map(Shortcut::default_for)
                .find(|s| {
                    ![
                        &config.clipboard_shortcut,
                        &config.mouse_shortcut,
                        &config.settings_shortcut,
                    ]
                    .contains(&s)
                })
                .unwrap();
        }
        // A shortcut added later must not take one already chosen.
        if document.get("chat_shortcut").is_none() {
            config.chat_shortcut = ["KeyT", "KeyH", "F10"]
                .into_iter()
                .map(Shortcut::default_for)
                .find(|s| {
                    ![
                        &config.clipboard_shortcut,
                        &config.mouse_shortcut,
                        &config.game_boost_shortcut,
                        &config.settings_shortcut,
                    ]
                    .contains(&s)
                })
                .unwrap();
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let shortcuts = [
            &self.clipboard_shortcut,
            &self.mouse_shortcut,
            &self.game_boost_shortcut,
            &self.settings_shortcut,
            &self.chat_shortcut,
        ];
        if shortcuts.iter().any(|s| !s.valid()) {
            bail!("Shortcuts need Ctrl or Alt plus a letter or F1-F12.");
        }
        if shortcuts
            .iter()
            .enumerate()
            .any(|(i, s)| shortcuts[..i].contains(s))
        {
            bail!("Clipboard, mouse, Game Boost and Settings shortcuts must be different.");
        }
        Ok(())
    }

    pub fn save(&self) -> Result<()> {
        self.validate()?;
        self.save_to(&Self::path()?)
    }

    fn save_to(&self, path: &std::path::Path) -> Result<()> {
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, toml::to_string_pretty(self)?).context("writing viewer settings")?;
        std::fs::rename(&tmp, path).context("saving viewer settings")
    }
}

/// How often an open editor looks for changes made elsewhere, such as a
/// session's shortcuts. Only on frames it draws anyway, so an idle window
/// stays idle.
const RELOAD_EVERY: Duration = Duration::from_millis(500);

const ERROR_RED: egui::Color32 = egui::Color32::from_rgb(220, 60, 50);

/// The viewer settings, saved as they change; open sessions pick them up.
pub struct Editor {
    /// Where they live: `viewer.toml` outside tests.
    path: PathBuf,
    config: ViewerSettings,
    /// What the file holds as far as this editor knows: its last save or read.
    saved: ViewerSettings,
    /// Why the file could not be read; the first save replaces it.
    load_error: Option<String>,
    /// Why the current edit is not saved yet.
    save_error: Option<String>,
    /// The edit whose save failed; not tried again until it changes.
    failed: Option<ViewerSettings>,
    reloaded: Instant,
}

impl Default for Editor {
    fn default() -> Self {
        match ViewerSettings::path() {
            Ok(path) => Self::at(path),
            Err(e) => Self {
                load_error: Some(format!("Cannot read settings: {e:#}")),
                ..Self::at(PathBuf::new())
            },
        }
    }
}

impl Editor {
    fn at(path: PathBuf) -> Self {
        let (config, load_error) = match ViewerSettings::load_from(&path) {
            Ok(config) => (config, None),
            Err(e) => (
                ViewerSettings::default(),
                Some(format!(
                    "Cannot read settings: {e}. Changing one here replaces them."
                )),
            ),
        };
        Self {
            path,
            saved: config.clone(),
            config,
            load_error,
            save_error: None,
            failed: None,
            reloaded: Instant::now(),
        }
    }

    /// Saves the edits once they are valid, on top of what the file holds
    /// now, so a change made elsewhere since the last reload is kept; until
    /// then says why not. `true` when what it says changed.
    fn commit(&mut self) -> bool {
        if self.config == self.saved {
            // Nothing to save, including an invalid edit that was undone.
            self.failed = None;
            return self.save_error.take().is_some();
        }
        if self.failed.as_ref() == Some(&self.config) {
            return false; // waits for the next change
        }
        let current = ViewerSettings::load_from(&self.path).unwrap_or_else(|_| self.saved.clone());
        let merged = self.edits_onto(current);
        match merged.validate().and_then(|()| merged.save_to(&self.path)) {
            Ok(()) => {
                self.config = merged.clone();
                self.saved = merged;
                self.failed = None;
                self.load_error.take().is_some() | self.save_error.take().is_some()
            }
            Err(e) => {
                self.failed = Some(self.config.clone());
                self.save_error = Some(format!("Not saved: {e:#}"));
                true
            }
        }
    }

    /// The fields edited here (where `config` differs from `saved`), put
    /// onto `base`.
    fn edits_onto(&self, mut base: ViewerSettings) -> ViewerSettings {
        macro_rules! edited {
            ($($field:ident),*) => {{
                // Names every field: a new one does not compile until listed.
                let ViewerSettings { $($field: _),* } = &self.config;
                $(if self.config.$field != self.saved.$field {
                    base.$field = self.config.$field.clone();
                })*
            }};
        }
        edited!(
            clipboard,
            mouse,
            allow_files,
            game_boost,
            clipboard_shortcut,
            mouse_shortcut,
            game_boost_shortcut,
            settings_shortcut,
            chat_shortcut,
            rendezvous_server
        );
        base
    }

    /// Picks up changes made elsewhere (a session's shortcuts, another
    /// settings window), unless an edit here is still waiting to be saved.
    fn reload(&mut self) {
        if self.config != self.saved {
            return;
        }
        let Ok(on_disk) = ViewerSettings::load_from(&self.path) else {
            return;
        };
        if on_disk == self.saved {
            return;
        }
        self.config = on_disk.clone();
        self.saved = on_disk;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if self.reloaded.elapsed() >= RELOAD_EVERY {
            self.reloaded = Instant::now();
            self.reload();
        }
        ui.heading("Viewer Settings");
        ui.small("Changes are saved at once and apply to open sessions.");
        for problem in [&self.load_error, &self.save_error].into_iter().flatten() {
            ui.colored_label(ERROR_RED, problem);
        }
        ui.add_space(8.0);
        let boost_label = if self.config.game_boost {
            "Game Boost: ON (click to return to Desktop)"
        } else {
            "Game Boost: OFF (click to enable)"
        };
        if ui.button(boost_label).clicked() {
            self.config.game_boost = !self.config.game_boost;
        }
        ui.small("Experimental: 60 FPS target, motion preset and smaller audio/video buffers.");
        ui.small(
            "Keeps host resolution and bitrate. Software encoding; actual FPS depends on both PCs.",
        );
        ui.small("Keyboard and desktop mouse supported. Relative game-camera input is not implemented yet.");
        ui.add_space(8.0);
        ui.checkbox(
            &mut self.config.clipboard,
            "Share text clipboard in both directions",
        );
        ui.small("Off by default. The host must also allow clipboard sharing.");
        ui.small("Only new copies are shared after enabling. Text only, up to 48 KiB.");
        ui.add_space(8.0);
        ui.checkbox(&mut self.config.mouse, "Control the host mouse");
        ui.small("Includes movement, buttons and scrolling.");
        ui.add_space(8.0);
        ui.checkbox(&mut self.config.allow_files, "Allow files from the host");
        ui.small("Saved in Downloads\\TideDesk on this computer.");
        ui.small("After local host movement, your next movement picks up its position without moving the host pointer.");
        ui.separator();
        ui.label("Shortcuts (while the remote window is focused)");
        shortcut_ui(ui, "Clipboard", &mut self.config.clipboard_shortcut);
        shortcut_ui(ui, "Mouse", &mut self.config.mouse_shortcut);
        shortcut_ui(ui, "Game Boost", &mut self.config.game_boost_shortcut);
        shortcut_ui(ui, "Settings", &mut self.config.settings_shortcut);
        shortcut_ui(ui, "Chat", &mut self.config.chat_shortcut);
        ui.small("Settings opens these settings during a session.");
        ui.separator();
        // The problems are drawn above: show a change on the next frame.
        if self.commit() {
            ui.ctx().request_repaint();
        }
    }
}

fn shortcut_ui(ui: &mut egui::Ui, label: &str, shortcut: &mut Shortcut) {
    ui.push_id(label, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(label);
            ui.checkbox(&mut shortcut.ctrl, "Ctrl");
            ui.checkbox(&mut shortcut.alt, "Alt");
            ui.checkbox(&mut shortcut.shift, "Shift");
            egui::ComboBox::from_id_salt("key")
                .selected_text(shortcut.key.strip_prefix("Key").unwrap_or(&shortcut.key))
                .width(55.0)
                .show_ui(ui, |ui| {
                    for key in keys() {
                        let label = key.strip_prefix("Key").unwrap_or(&key).to_owned();
                        ui.selectable_value(&mut shortcut.key, key, label);
                    }
                });
        });
    });
}

/// The settings window's size, in points.
const SIZE: [f32; 2] = [550.0, 590.0];

/// Where a session asks the settings window to open: the centre of its own
/// window, in points, as `x,y`.
pub fn parse_near(text: &str) -> Option<[f32; 2]> {
    let (x, y) = text.split_once(',')?;
    let (x, y) = (x.trim().parse::<f32>().ok()?, y.trim().parse::<f32>().ok()?);
    (x.is_finite() && y.is_finite()).then_some([x, y])
}

/// The top left corner that centres the settings window on `centre`.
fn centred_on(centre: [f32; 2]) -> [f32; 2] {
    [centre[0] - SIZE[0] / 2.0, centre[1] - SIZE[1] / 2.0]
}

/// Opens the settings window, centred on `near` when a session gives it:
/// in front of the session, on its monitor, rather than where Windows puts
/// new windows.
pub fn run(near: Option<[f32; 2]>) -> Result<()> {
    struct Window(Editor);
    impl egui_software_backend::App for Window {
        fn ui(&mut self, ui: &mut egui::Ui, _: &mut egui_software_backend::SoftwareBackend) {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.0.ui(ui));
            });
        }
    }
    let mut config = egui_software_backend::SoftwareBackendAppConfiguration::new();
    config.viewport_builder = egui::ViewportBuilder::default()
        .with_title("TideDesk Viewer Settings")
        .with_inner_size(SIZE)
        .with_min_inner_size([420.0, 350.0])
        .with_icon(crate::icon::egui_icon());
    if let Some(centre) = near {
        config.viewport_builder = config.viewport_builder.with_position(centred_on(centre));
    }
    egui_software_backend::run_app_with_software_backend(config, |_| Window(Editor::default()))
        .map_err(|e| anyhow::anyhow!("cannot open settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    /// A session gives the centre of its window; settings open centred on it.
    #[test]
    fn settings_open_centred_on_the_session() {
        assert_eq!(parse_near("3200,1080"), Some([3200.0, 1080.0]));
        assert_eq!(parse_near(" -1280.5 , 300 "), Some([-1280.5, 300.0]));
        for bad in ["", "3200", "x,1", "1,NaN", "1,inf"] {
            assert_eq!(parse_near(bad), None, "{bad}");
        }
        assert_eq!(centred_on([3200.0, 1080.0]), [2925.0, 785.0]);
    }

    /// Settings from before the chat keep their shortcuts; the chat takes
    /// the first free one.
    #[test]
    fn the_chat_shortcut_does_not_take_one_already_chosen() {
        let fresh = ViewerSettings::parse("mouse = true").unwrap();
        assert_eq!(fresh.chat_shortcut, Shortcut::default_for("KeyT"));
        let taken = "[mouse_shortcut]
ctrl = true
alt = true
shift = false
key = \"KeyT\"
";
        let older = ViewerSettings::parse(taken).unwrap();
        assert_eq!(older.mouse_shortcut, Shortcut::default_for("KeyT"));
        assert_eq!(older.chat_shortcut, Shortcut::default_for("KeyH"));
    }

    #[test]
    fn defaults_and_old_partial_configs() {
        let config: ViewerSettings = toml::from_str("mouse = false").unwrap();
        assert!(!config.clipboard);
        assert!(!config.mouse);
        assert!(!config.game_boost);
        assert!(ViewerSettings::default().mouse);
        assert_eq!(
            config.rendezvous_server, "",
            "empty means TideDesk's own service; the default is not saved"
        );
        config.validate().unwrap();
        let back: ViewerSettings = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn shortcut_modifiers_are_exact_and_duplicates_are_rejected() {
        let mut config = ViewerSettings::default();
        let shortcut = &config.clipboard_shortcut;
        assert!(shortcut.matches(
            ModifiersState::CONTROL | ModifiersState::ALT,
            PhysicalKey::Code(KeyCode::KeyC)
        ));
        assert!(!shortcut.matches(ModifiersState::CONTROL, PhysicalKey::Code(KeyCode::KeyC)));
        assert!(!shortcut.matches(
            ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT,
            PhysicalKey::Code(KeyCode::KeyC)
        ));
        config.mouse_shortcut = config.clipboard_shortcut.clone();
        assert!(config.validate().is_err());
        config.mouse_shortcut = config.settings_shortcut.clone();
        assert!(config.validate().is_err());
    }

    #[test]
    fn saving_replaces_existing_settings_atomically() {
        let path =
            std::env::temp_dir().join(format!("tidedesk-viewer-test-{}.toml", std::process::id()));
        let mut config = ViewerSettings::default();
        config.save_to(&path).unwrap();
        config.clipboard = true;
        config.game_boost = true;
        config.save_to(&path).unwrap();
        let saved: ViewerSettings =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, config);
        std::fs::remove_file(path).unwrap();
    }

    fn temp_path(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tidedesk-viewer-{name}-{}.toml",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn valid_edits_are_saved_at_once_and_invalid_ones_wait() {
        let path = temp_path("autosave");
        let mut editor = Editor::at(path.clone());
        editor.commit();
        assert!(!path.exists(), "nothing changed, nothing written");

        editor.config.clipboard = true;
        editor.commit();
        assert!(ViewerSettings::load_from(&path).unwrap().clipboard);
        assert_eq!(editor.save_error, None);

        // Half-way through swapping two shortcuts: shown, not saved.
        editor.config.mouse_shortcut = editor.config.clipboard_shortcut.clone();
        editor.commit();
        let why = editor.save_error.clone().expect("the reason is shown");
        assert!(why.contains("must be different"), "{why}");
        let saved = ViewerSettings::load_from(&path).unwrap();
        assert_eq!(saved.mouse_shortcut, Shortcut::default_for("KeyM"));

        editor.config.clipboard_shortcut = Shortcut::default_for("KeyK");
        editor.commit();
        assert_eq!(editor.save_error, None);
        let saved = ViewerSettings::load_from(&path).unwrap();
        assert_eq!(saved.clipboard_shortcut.key, "KeyK");
        assert_eq!(saved.mouse_shortcut.key, "KeyC");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn changes_made_elsewhere_are_picked_up_unless_an_edit_is_pending() {
        let path = temp_path("elsewhere");
        let mut editor = Editor::at(path.clone());
        // A session toggles Game Boost with its shortcut.
        let mut session = ViewerSettings {
            game_boost: true,
            ..Default::default()
        };
        session.save_to(&path).unwrap();
        editor.reload();
        assert!(editor.config.game_boost);
        editor.commit();
        assert_eq!(ViewerSettings::load_from(&path).unwrap(), session);

        // An edit that cannot be saved yet is not thrown away.
        editor.config.mouse_shortcut = editor.config.clipboard_shortcut.clone();
        session.clipboard = true;
        session.save_to(&path).unwrap();
        editor.reload();
        assert_eq!(
            editor.config.mouse_shortcut,
            editor.config.clipboard_shortcut
        );
        assert!(!editor.config.clipboard);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_change_made_elsewhere_while_editing_is_kept() {
        let path = temp_path("merge");
        let mut editor = Editor::at(path.clone());
        // Half-way through swapping two shortcuts, so nothing is reloaded…
        editor.config.mouse_shortcut = editor.config.clipboard_shortcut.clone();
        editor.commit();
        // …while a session's shortcut turns Game Boost on.
        let session = ViewerSettings {
            game_boost: true,
            ..Default::default()
        };
        session.save_to(&path).unwrap();
        editor.reload();

        editor.config.clipboard_shortcut = Shortcut::default_for("KeyK");
        editor.commit();
        assert_eq!(editor.save_error, None);
        let saved = ViewerSettings::load_from(&path).unwrap();
        assert!(saved.game_boost, "the session's change survives");
        assert_eq!(saved.clipboard_shortcut.key, "KeyK");
        assert_eq!(saved.mouse_shortcut.key, "KeyC");
        assert_eq!(editor.config, saved, "and shows here");
        std::fs::remove_file(path).unwrap();
    }

    /// A connection service named in the file is not shown in the settings,
    /// and kept when they change.
    #[test]
    fn a_connection_service_named_in_the_file_is_kept() {
        let path = temp_path("service");
        let named = ViewerSettings {
            rendezvous_server: "rv.example:47900".into(),
            ..Default::default()
        };
        named.save_to(&path).unwrap();
        let mut editor = Editor::at(path.clone());
        editor.config.clipboard = true;
        editor.commit();
        let saved = ViewerSettings::load_from(&path).unwrap();
        assert!(saved.clipboard);
        assert_eq!(saved.rendezvous_server, "rv.example:47900");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn old_custom_bindings_survive_boost_shortcut_migration() {
        let config = ViewerSettings::parse(
            "mouse = false\n[mouse_shortcut]\nctrl=true\nalt=true\nshift=false\nkey='KeyG'\n",
        )
        .unwrap();
        assert!(!config.mouse);
        assert_eq!(config.mouse_shortcut.key, "KeyG");
        assert_eq!(config.game_boost_shortcut.key, "KeyB");
        let mut duplicate = config;
        duplicate.game_boost_shortcut = duplicate.mouse_shortcut.clone();
        assert!(duplicate.validate().is_err());
        duplicate.game_boost_shortcut = duplicate.settings_shortcut.clone();
        assert!(duplicate.validate().is_err());
    }
}
