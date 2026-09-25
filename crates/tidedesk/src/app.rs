//! The one window: **Share this computer**, **Connect to a computer** and
//! **Settings** as tabs. Sharing runs in this process, as in the host window
//! it replaces; connecting starts a session process, as the connect window
//! it replaces did.

use anyhow::{Result, anyhow};
use egui::RichText;
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_host::{HostApp, StartOptions, Started};
use tidedesk_view::launcher::Launcher;
use tidedesk_view::settings::Editor;

/// Also the name tray and taskbar handling find the window by.
pub const WINDOW_TITLE: &str = "TideDesk";

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

struct Shell {
    tab: Tab,
    /// The sharing side, or why it could not start (its port in use, say):
    /// connecting still works then.
    host: Result<HostApp, String>,
    /// Runs the sharing side; dropping it would stop sharing.
    _runtime: Option<tokio::runtime::Runtime>,
    launcher: Launcher,
    viewer_settings: Editor,
}

/// Opens the window, hidden in the tray when `hidden` or the settings say so.
pub fn run(hidden: bool) -> Result<()> {
    tidedesk_view::set_self_prefix(&["view"]);
    let options = StartOptions {
        tray: hidden,
        ..StartOptions::default()
    };
    let (host, runtime, start_hidden, show_in_taskbar) = match tidedesk_host::start(&options) {
        Ok(Started { info, runtime, .. }) => {
            let (start_hidden, taskbar) = (info.start_hidden, info.config.show_in_taskbar);
            (
                Ok(HostApp::new(info, WINDOW_TITLE)),
                Some(runtime),
                start_hidden,
                taskbar,
            )
        }
        Err(e) => {
            tracing::warn!("sharing could not start: {e:#}");
            (Err(format!("{e:#}")), None, hidden, true)
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
        host,
        _runtime: runtime,
        launcher: Launcher::new(),
        viewer_settings: Editor::default(),
    });
    egui_software_backend::run_app_with_software_backend(config, move |ctx| {
        let mut shell = shell.take().expect("the window is created once");
        if let Ok(host) = &mut shell.host {
            host.attach(ctx);
        }
        shell
    })
    .map_err(|e| anyhow!("cannot open the TideDesk window: {e}"))
}

impl Shell {
    fn share_tab(&mut self, ui: &mut egui::Ui) {
        match &mut self.host {
            Ok(host) => host.status_tab(ui),
            Err(problem) => {
                ui.label(RichText::new("Sharing could not start").strong());
                ui.label(problem.as_str());
                ui.small(
                    "Another TideDesk may already be sharing this computer: close it and start \
                     TideDesk again, or change the port under Settings. Connecting to other \
                     computers still works.",
                );
            }
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        if let Ok(host) = &mut self.host {
            host.settings_tab(ui);
            ui.add_space(8.0);
            ui.separator();
        }
        self.viewer_settings.ui(ui);
    }
}

impl egui_software_backend::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut SoftwareBackend) {
        if let Ok(host) = &mut self.host {
            host.frame();
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
            Tab::Connect => self.launcher.ui_in(ui),
            Tab::Settings => {
                egui::ScrollArea::vertical().show(ui, |ui| self.settings_tab(ui));
            }
        });
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
