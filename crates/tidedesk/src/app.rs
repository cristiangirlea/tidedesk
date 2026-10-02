//! The one window, with its sections in a sidebar: **This computer**,
//! **Connect**, **History**, **Settings** and **About**. Sharing runs in this process, as in the host window
//! it replaces; connecting starts a session process, as the connect window
//! it replaces did. A new installation first shows the terms, and shares
//! nothing until they are accepted.

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use egui::RichText;
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_core::licence::{self, Licence, Standing};
use tidedesk_host::{HostApp, StartOptions, Started};
use tidedesk_view::launcher::Launcher;
use tidedesk_view::settings::Editor;

use crate::terms::{self, Block, Consent};

/// Also the name tray and taskbar handling find the window by.
pub const WINDOW_TITLE: &str = "TideDesk";

const RELEASES_URL: &str = "https://github.com/cristiangirlea/tidedesk/releases";
const PRIVACY_URL: &str =
    "https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md#privacy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Share,
    Connect,
    History,
    Settings,
    About,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Share,
        Tab::Connect,
        Tab::History,
        Tab::Settings,
        Tab::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Share => "This computer",
            Tab::Connect => "Connect",
            Tab::History => "History",
            Tab::Settings => "Settings",
            Tab::About => "About",
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
    licence: LicenceBox,
    /// What the session history is filtered by, and what its export did.
    history_search: String,
    history_note: Option<String>,
    /// TideDesk's icon, at the top of the sidebar.
    logo: Option<egui::TextureHandle>,
}

/// The licence part of About: the licence this computer holds, and a box to
/// paste one in.
#[derive(Default)]
struct LicenceBox {
    held: Option<Licence>,
    pasted: String,
    /// What became of the last licence pasted: what was added, or why not.
    outcome: Option<Result<String, String>>,
}

/// What About says about a licence on `today`.
fn licence_line(licence: &Licence, today: &str) -> String {
    let (name, edition) = (&licence.licensee, &licence.edition);
    let expires = licence.expires.as_deref().unwrap_or_default();
    match licence.standing(today) {
        Standing::Active if licence.expires.is_none() => {
            format!("Licensed to {name}: {edition}.")
        }
        Standing::Active => format!("Licensed to {name}: {edition}, until {expires}."),
        Standing::Grace { until } => format!(
            "Licensed to {name}: {edition}. It expired on {expires} and keeps working until \
             {until}: renew it before then."
        ),
        Standing::Expired => {
            format!("The {edition} licence for {name} expired on {expires}. Renew it to go on.")
        }
    }
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
        .with_inner_size([880.0, 660.0])
        .with_min_inner_size([720.0, 520.0])
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
        licence: LicenceBox {
            held: licence::load(),
            ..LicenceBox::default()
        },
        history_search: String::new(),
        history_note: None,
        logo: None,
    });
    egui_software_backend::run_app_with_software_backend(config, move |ctx| {
        let mut shell = shell.take().expect("the window is created once");
        tidedesk_ui::apply(&ctx);
        let icon = tidedesk_host::window_icon();
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        );
        shell.logo = Some(ctx.load_texture("tidedesk-logo", image, Default::default()));
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
    }

    fn about(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("TideDesk").size(22.0).strong());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Version {}", crate::VERSION)).size(16.0));
            copy_version(ui);
        });
        ui.small("Remote access to your own computers.");
        if ui.button("Is there a newer version?").clicked() {
            tidedesk_host::open_link(RELEASES_URL);
        }
        ui.add_space(8.0);
        if let Some(held) = &self.licence.held {
            let today = tidedesk_core::dates::today();
            let line = licence_line(held, &today);
            match held.standing(&today) {
                Standing::Active => ui.label(RichText::new(line).strong()),
                _ => ui.colored_label(ui.visuals().warn_fg_color, line),
            };
        }
        if let Some(declared) = tidedesk_core::company::declaration() {
            ui.small(format!("This computer: {}", declared.describe()));
        }
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
        ui.add_space(8.0);
        self.licence_box(ui);
    }

    fn licence_box(&mut self, ui: &mut egui::Ui) {
        let title = if self.licence.held.is_some() {
            "Replace the licence"
        } else {
            "Add a licence"
        };
        egui::CollapsingHeader::new(title)
            .id_salt("licence")
            .show(ui, |ui| {
                ui.small("Paste the licence from your email, from its BEGIN line to its END line.");
                let edited = ui
                    .add(
                        egui::TextEdit::multiline(&mut self.licence.pasted)
                            .desired_rows(6)
                            .desired_width(f32::INFINITY)
                            .code_editor(),
                    )
                    .changed();
                if edited {
                    self.licence.outcome = None;
                }
                let pasted = !self.licence.pasted.trim().is_empty();
                if ui.add_enabled(pasted, egui::Button::new("Add")).clicked() {
                    self.licence.outcome = Some(match licence::add(&self.licence.pasted) {
                        Ok(added) => {
                            let line = licence_line(&added, &tidedesk_core::dates::today());
                            self.licence.held = Some(added);
                            self.licence.pasted.clear();
                            Ok(format!("Licence added. {line}"))
                        }
                        Err(e) => Err(format!("{e:#}")),
                    });
                }
                match &self.licence.outcome {
                    Some(Ok(added)) => {
                        ui.label(added);
                    }
                    Some(Err(why)) => {
                        ui.colored_label(ui.visuals().error_fg_color, why);
                    }
                    None => {}
                }
            });
    }

    /// The sections on the left, TideDesk's name on top and the edition
    /// at the bottom.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        use tidedesk_ui as look;
        let frame = egui::Frame::new()
            .fill(look::SIDEBAR)
            .inner_margin(egui::Margin::symmetric(12, 18));
        egui::Panel::left("sections")
            .exact_size(200.0)
            .resizable(false)
            .frame(frame)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new(logo).fit_to_exact_size(egui::vec2(28.0, 28.0)));
                    }
                    ui.label(look::label("TideDesk").size(17.0));
                });
                ui.add_space(14.0);
                for tab in Tab::ALL {
                    let selected = self.tab == tab;
                    let text = egui::RichText::new(tab.label())
                        .size(15.0)
                        .color(if selected { look::TEXT } else { look::MUTED });
                    let item = egui::Button::new(text)
                        .fill(if selected {
                            look::SELECTED
                        } else {
                            look::SIDEBAR
                        })
                        .stroke(egui::Stroke::NONE)
                        .min_size(egui::vec2(ui.available_width(), 38.0));
                    if ui.add(item).clicked() {
                        self.tab = tab;
                    }
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    egui::Frame::new()
                        .fill(look::SURFACE)
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let edition = match &self.licence.held {
                                Some(held) => held.edition.clone(),
                                None => "Personal, free".into(),
                            };
                            ui.label(look::label(&edition));
                            ui.label(egui::RichText::new("Edition").small().color(look::MUTED));
                        });
                });
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

/// A button that copies the version, for a question or a report.
fn copy_version(ui: &mut egui::Ui) {
    if ui
        .small_button("Copy")
        .on_hover_text("Copies the version, for a question or a problem report.")
        .clicked()
    {
        ui.ctx().copy_text(format!("TideDesk {}", crate::VERSION));
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
        self.sidebar(ui);
        let page = egui::Frame::new()
            .fill(tidedesk_ui::BG)
            .inner_margin(egui::Margin::symmetric(28, 22));
        egui::CentralPanel::default()
            .frame(page)
            .show_inside(ui, |ui| match self.tab {
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
                Tab::History => {
                    ui.label(tidedesk_ui::title("History"));
                    ui.label(
                        egui::RichText::new("Who connected to this computer.")
                            .color(tidedesk_ui::MUTED),
                    );
                    ui.add_space(6.0);
                    tidedesk_host::session_history(
                        ui,
                        &mut self.history_search,
                        &mut self.history_note,
                    );
                }
                Tab::Settings => {
                    egui::ScrollArea::vertical().show(ui, |ui| self.settings_tab(ui));
                }
                Tab::About => {
                    egui::ScrollArea::vertical().show(ui, |ui| self.about(ui));
                }
            });
        let ctx = ui.ctx().clone();
        self.reading_window(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::{Licence, Tab, licence_line};

    #[test]
    fn the_window_opens_on_sharing_and_names_its_tabs() {
        assert_eq!(Tab::default(), Tab::Share);
        let labels: Vec<_> = Tab::ALL.iter().map(|t| t.label()).collect();
        assert_eq!(
            labels,
            ["This computer", "Connect", "History", "Settings", "About"]
        );
    }

    #[test]
    fn about_says_what_a_licence_allows_and_until_when() {
        let licence = Licence {
            licensee: "Ana Pop".into(),
            email: "ana@example.com".into(),
            edition: "Pro".into(),
            features: vec!["work".into()],
            seats: 1,
            issued: "2026-10-02".into(),
            expires: Some("2027-10-02".into()),
        };
        assert_eq!(
            licence_line(&licence, "2027-10-02"),
            "Licensed to Ana Pop: Pro, until 2027-10-02."
        );
        assert_eq!(
            licence_line(&licence, "2027-10-05"),
            "Licensed to Ana Pop: Pro. It expired on 2027-10-02 and keeps working until \
             2027-10-16: renew it before then."
        );
        assert_eq!(
            licence_line(&licence, "2027-10-17"),
            "The Pro licence for Ana Pop expired on 2027-10-02. Renew it to go on."
        );
        let forever = Licence {
            expires: None,
            ..licence
        };
        assert_eq!(
            licence_line(&forever, "2099-01-01"),
            "Licensed to Ana Pop: Pro."
        );
    }
}
