//! Saved viewer controls and editable session shortcuts.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
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

pub fn settings_shortcut() -> Shortcut {
    Shortcut::default_for("KeyS")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSettings {
    pub clipboard: bool,
    pub mouse: bool,
    pub game_boost: bool,
    pub clipboard_shortcut: Shortcut,
    pub mouse_shortcut: Shortcut,
    pub game_boost_shortcut: Shortcut,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            clipboard: false,
            mouse: true,
            game_boost: false,
            clipboard_shortcut: Shortcut::default_for("KeyC"),
            mouse_shortcut: Shortcut::default_for("KeyM"),
            game_boost_shortcut: Shortcut::default_for("KeyG"),
        }
    }
}

impl ViewerSettings {
    pub fn path() -> Result<PathBuf> {
        Ok(tidedesk_core::paths::config_dir()?.join("viewer.toml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
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
                .find(|s| *s != config.clipboard_shortcut && *s != config.mouse_shortcut)
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
        ];
        if shortcuts.iter().any(|s| !s.valid()) {
            bail!("Shortcuts need Ctrl or Alt plus a letter or F1-F12.");
        }
        if shortcuts
            .iter()
            .enumerate()
            .any(|(i, s)| shortcuts[..i].contains(s))
        {
            bail!("Clipboard, mouse and Game Boost shortcuts must be different.");
        }
        if shortcuts.contains(&&settings_shortcut()) {
            bail!("Ctrl+Alt+S is reserved for Viewer Settings.");
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

pub struct Editor {
    config: ViewerSettings,
    message: Option<String>,
}

impl Default for Editor {
    fn default() -> Self {
        match ViewerSettings::load() {
            Ok(config) => Self {
                config,
                message: None,
            },
            Err(e) => Self {
                config: ViewerSettings::default(),
                message: Some(format!("Cannot read settings: {e}")),
            },
        }
    }
}

impl Editor {
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Viewer Settings");
        ui.add_space(8.0);
        let boost_label = if self.config.game_boost {
            "Game Boost: ON — return to Desktop"
        } else {
            "Game Boost: OFF — enable"
        };
        if ui.button(boost_label).clicked() {
            self.config.game_boost = !self.config.game_boost;
            match self.config.save() {
                Ok(()) => self.message = Some("Saved. Applying to open sessions.".into()),
                Err(e) => {
                    self.config.game_boost = !self.config.game_boost;
                    self.message = Some(format!("Cannot save: {e}"));
                }
            }
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
        ui.small("After local host movement, your next movement picks up its position without moving the host pointer.");
        ui.separator();
        ui.label("Shortcuts (while the remote window is focused)");
        shortcut_ui(ui, "Clipboard", &mut self.config.clipboard_shortcut);
        shortcut_ui(ui, "Mouse", &mut self.config.mouse_shortcut);
        shortcut_ui(ui, "Game Boost", &mut self.config.game_boost_shortcut);
        ui.small("Ctrl+Alt+S opens these settings during a session.");
        ui.add_space(8.0);
        if ui.button("Save settings").clicked() {
            self.message = Some(match self.config.save() {
                Ok(()) => "Saved. Changes apply to open sessions immediately.".into(),
                Err(e) => format!("Cannot save: {e}"),
            });
        }
        if let Some(message) = &self.message {
            ui.label(message);
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

pub fn run() -> Result<()> {
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
        .with_inner_size([550.0, 590.0])
        .with_min_inner_size([420.0, 350.0])
        .with_icon(crate::icon::egui_icon());
    egui_software_backend::run_app_with_software_backend(config, |_| Window(Editor::default()))
        .map_err(|e| anyhow::anyhow!("cannot open settings: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn defaults_and_old_partial_configs() {
        let config: ViewerSettings = toml::from_str("mouse = false").unwrap();
        assert!(!config.clipboard);
        assert!(!config.mouse);
        assert!(!config.game_boost);
        assert!(ViewerSettings::default().mouse);
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
        config.mouse_shortcut = settings_shortcut();
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
        duplicate.game_boost_shortcut = settings_shortcut();
        assert!(duplicate.validate().is_err());
    }
}
