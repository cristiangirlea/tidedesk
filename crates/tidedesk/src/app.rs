//! The one window: **Share this computer**, **Connect to a computer** and
//! **Settings** as tabs. Sharing runs in this process, as in the host window
//! it replaces; connecting starts a session process, as the connect window
//! it replaces did. A new installation first shows the terms, and shares
//! nothing until they are accepted.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use egui::RichText;
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_host::{HostApp, StartOptions, Started};
use tidedesk_view::launcher::Launcher;
use tidedesk_view::settings::Editor;

use crate::terms::{self, Block, Consent};

/// Also the name tray and taskbar handling find the window by.
pub const WINDOW_TITLE: &str = "TideDesk";

const PRIVACY_URL: &str =
    "https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md#privacy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Share,
    Connect,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Share, Tab::Connect, Tab::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Share => "Share this computer",
            Tab::Connect => "Connect to a computer",
            Tab::Settings => "Settings",
        }
    }
}

/// Sharing this computer, once started.
struct Sharing {
    /// The sharing side, or why it could not start (its port in use, say):
    /// connecting still works then.
    host: Result<HostApp, String>,
    /// Runs the sharing side; dropping it would stop sharing.
    _runtime: Option<tokio::runtime::Runtime>,
}

/// A text open in its own window.
struct Reading {
    title: &'static str,
    text: &'static [Block],
}

impl Reading {
    fn license() -> Self {
        Self {
            title: "License",
            text: &terms::LICENSE,
        }
    }

    fn terms() -> Self {
        Self {
            title: "Terms of use",
            text: &terms::TERMS,
        }
    }
}

struct Shell {
    tab: Tab,
    /// `None` until a new installation accepts the terms.
    sharing: Option<Sharing>,
    consent: Consent,
    /// Where accepting the terms is recorded.
    config_dir: Option<PathBuf>,
    /// The components' license notices, where the package put them.
    notices: Option<PathBuf>,
    reading: Option<Reading>,
    launcher: Launcher,
    viewer_settings: Editor,
}

/// Starts sharing, in the tray when `hidden`. Also says whether the window
/// starts hidden and whether it has a taskbar button.
fn start_sharing(hidden: bool) -> (Sharing, bool, bool) {
    let options = StartOptions {
        tray: hidden,
        ..StartOptions::default()
    };
    match tidedesk_host::start(&options) {
        Ok(Started { info, runtime, .. }) => {
            let (start_hidden, taskbar) = (info.start_hidden, info.config.show_in_taskbar);
            let sharing = Sharing {
                host: Ok(HostApp::new(info, WINDOW_TITLE)),
                _runtime: Some(runtime),
            };
            (sharing, start_hidden, taskbar)
        }
        Err(e) => {
            tracing::warn!("sharing could not start: {e:#}");
            // No tray icon without sharing, so never start hidden: the window
            // would be unreachable.
            let sharing = Sharing {
                host: Err(format!("{e:#}")),
                _runtime: None,
            };
            (sharing, false, true)
        }
    }
}

/// The components' license notices, next to the program in the ZIP and in
/// the Store package.
fn third_party_notices() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let index = exe
        .parent()?
        .join("licenses")
        .join("third-party")
        .join("INDEX.txt");
    index.is_file().then_some(index)
}

/// Opens the window, hidden in the tray when `hidden` or the settings say so.
pub fn run(hidden: bool) -> Result<()> {
    tidedesk_view::set_self_prefix(&["view"]);
    let config_dir = tidedesk_core::paths::config_dir().ok();
    let consent = config_dir.as_deref().map_or(Consent::New, terms::load);
    // A new installation shares nothing before the terms are accepted, in a
    // window that is shown.
    let (sharing, start_hidden, show_in_taskbar) = match consent {
        Consent::New => (None, false, true),
        Consent::Accepted | Consent::Changed => {
            let (sharing, start_hidden, taskbar) = start_sharing(hidden);
            (Some(sharing), start_hidden, taskbar)
        }
    };
    let mut config = SoftwareBackendAppConfiguration::new();
    config.viewport_builder = egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_icon(tidedesk_host::window_icon())
        .with_inner_size([520.0, 620.0])
        .with_min_inner_size([420.0, 480.0])
        .with_taskbar(show_in_taskbar)
        .with_visible(!start_hidden);
    let mut shell = Some(Shell {
        tab: Tab::default(),
        sharing,
        consent,
        config_dir,
        notices: third_party_notices(),
        reading: None,
        launcher: Launcher::new(),
        viewer_settings: Editor::default(),
    });
    egui_software_backend::run_app_with_software_backend(config, move |ctx| {
        let mut shell = shell.take().expect("the window is created once");
        if let Some(host) = shell.host() {
            host.attach(ctx);
        }
        shell
    })
    .map_err(|e| anyhow!("cannot open the TideDesk window: {e}"))
}

impl Shell {
    fn host(&mut self) -> Option<&mut HostApp> {
        self.sharing.as_mut().and_then(|s| s.host.as_mut().ok())
    }

    /// Records that the terms were accepted; sharing starts if it had not.
    fn accept(&mut self, ctx: &egui::Context) {
        if let Some(dir) = &self.config_dir
            && let Err(e) = terms::accept(dir)
        {
            tracing::warn!("could not record that the terms were accepted: {e}");
        }
        self.consent = Consent::Accepted;
        if self.sharing.is_none() {
            let (sharing, _, _) = start_sharing(false);
            self.sharing = Some(sharing);
            if let Some(host) = self.host() {
                host.attach(ctx.clone());
            }
        }
        // The tabs replace this screen on the next frame.
        ctx.request_repaint();
    }

    /// A new installation's first screen.
    fn acceptance(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("accept").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Accept").strong()).clicked() {
                    self.accept(ui.ctx());
                }
                if ui.button("Decline and quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Welcome to TideDesk");
            ui.label(
                "TideDesk is free for personal, non-commercial use. Before this computer shares \
                 its screen or connects to another, read and accept the terms of use, which \
                 include the rules for TideDesk's connection service, and the license.",
            );
            ui.add_space(6.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::CollapsingHeader::new("Terms of use")
                    .default_open(true)
                    .show(ui, |ui| terms::show(ui, &terms::TERMS));
                egui::CollapsingHeader::new("License")
                    .show(ui, |ui| terms::show(ui, &terms::LICENSE));
            });
        });
    }

    /// For an installation used before these terms: it keeps working.
    fn terms_notice(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("terms-notice").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.label(
                "TideDesk now has terms of use, including rules for its connection service; \
                 the license is unchanged. By continuing to use TideDesk you agree to them.",
            );
            ui.horizontal(|ui| {
                if ui.button("Read the terms").clicked() {
                    self.reading = Some(Reading::terms());
                }
                if ui.button("OK").clicked() {
                    self.accept(ui.ctx());
                }
            });
            ui.add_space(4.0);
        });
    }

    fn share_tab(&mut self, ui: &mut egui::Ui) {
        match self.sharing.as_mut().map(|s| &mut s.host) {
            Some(Ok(host)) => host.status_tab(ui),
            Some(Err(problem)) => {
                ui.label(RichText::new("Sharing could not start").strong());
                ui.label(problem.as_str());
                ui.small(
                    "Another TideDesk may already be sharing this computer: close it and start \
                     TideDesk again, or change the port under Settings. Connecting to other \
                     computers still works.",
                );
            }
            // The tabs show once sharing has started.
            None => {}
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        if let Some(host) = self.host() {
            host.settings_tab(ui);
            ui.add_space(8.0);
            ui.separator();
        }
        self.viewer_settings.ui(ui);
        ui.add_space(8.0);
        ui.separator();
        self.about(ui);
    }

    fn about(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("About").strong());
        ui.label(format!("TideDesk {}", crate::VERSION));
        ui.small(
            "Free for personal, non-commercial use under the TideDesk Personal Use Source \
             License 1.0. Business use needs separate written permission.",
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button("License").clicked() {
                self.reading = Some(Reading::license());
            }
            if ui.button("Terms of use").clicked() {
                self.reading = Some(Reading::terms());
            }
            if ui.button("Privacy").clicked() {
                tidedesk_host::open_link(PRIVACY_URL);
            }
            if let Some(notices) = &self.notices
                && ui.button("Third-party notices").clicked()
            {
                tidedesk_host::open_link(&notices.to_string_lossy());
            }
        });
    }

    fn reading_window(&mut self, ctx: &egui::Context) {
        let Some(reading) = &self.reading else {
            return;
        };
        let mut open = true;
        egui::Window::new(reading.title)
            .open(&mut open)
            .resizable(true)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| terms::show(ui, reading.text));
            });
        if !open {
            self.reading = None;
        }
    }
}

impl egui_software_backend::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut SoftwareBackend) {
        if let Some(host) = self.host() {
            host.frame();
        }
        if self.consent == Consent::New {
            self.acceptance(ui);
            return;
        }
        if self.consent == Consent::Changed {
            self.terms_notice(ui);
        }
        egui::Panel::top("tabs").show_inside(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                for tab in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, tab.label());
                }
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show_inside(ui, |ui| match self.tab {
            Tab::Share => {
                egui::ScrollArea::vertical().show(ui, |ui| self.share_tab(ui));
            }
            Tab::Connect => {
                self.launcher.ui_in(ui);
                if self.launcher.settings_requested() {
                    self.viewer_settings = Editor::default();
                    self.tab = Tab::Settings;
                }
            }
            Tab::Settings => {
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_tab(ui));
            }
        });
        let ctx = ui.ctx().clone();
        self.reading_window(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::Tab;

    #[test]
    fn the_window_opens_on_sharing_and_names_its_tabs() {
        assert_eq!(Tab::default(), Tab::Share);
        let labels: Vec<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(
            labels,
            ["Share this computer", "Connect to a computer", "Settings"]
        );
    }
}
