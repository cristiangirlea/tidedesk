//! The host window: status, access code and addresses, plus a settings tab.
//!
//! Drawn on the CPU (egui + a software rasteriser) rather than through OpenGL
//! or Direct3D: a GPU context alone costs 150-500 MB on some drivers, while this
//! window needs a few MB. egui only repaints on input or when the server reports
//! a change, so an open window costs nothing while nothing happens.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use egui::{Color32, RichText};
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_core::nat::signal::Credentials;
use tidedesk_core::nat::signal::RendezvousStatus;
use tidedesk_core::nat::stun::STUN_REFRESH;
use tidedesk_core::nat::{Agent, NatKind, PublicStatus};

use crate::capture::{self, DisplayInfo};
use crate::config::HostConfig;
use crate::internet::{self, PathState};
use crate::session::HostState;
use crate::tray::Tray;
use crate::{icon, platform};

/// Also used to find the native window for tray and taskbar handling.
pub const WINDOW_TITLE: &str = "TideDesk Host";

const ERROR_RED: Color32 = Color32::from_rgb(220, 60, 50);

/// An address locked out for wrong access codes, as the host window says it.
fn blocked_line(blocked: &tidedesk_core::auth::Blocked) -> String {
    let seconds = blocked.for_another.as_secs().max(1);
    let wait = if seconds < 120 {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds.div_ceil(60))
    };
    format!(
        "{} wrong codes from {}: refused for {wait} more",
        blocked.failures, blocked.address
    )
}
const WARNING_AMBER: Color32 = Color32::from_rgb(180, 110, 0);

pub struct HostInfo {
    pub state: Arc<HostState>,
    pub agent: Arc<Agent>,
    /// For registering with a rendezvous service set in Settings.
    pub identity: Arc<Credentials>,
    /// Runs the agent's work started from the window.
    pub runtime: tokio::runtime::Handle,
    pub config: HostConfig,
    pub fingerprint: String,
    pub port: u16,
    pub start_hidden: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Status,
    Settings,
}

pub struct HostApp {
    info: HostInfo,
    /// The window title, by which tray and taskbar handling find the window.
    title: &'static str,
    tab: Tab,
    displays: Vec<DisplayInfo>,
    addresses: Vec<Address>,
    show_all_addresses: bool,
    /// A viewer's internet address as typed, and why it was not accepted.
    viewer_text: String,
    /// A password being set: the two fields, while open.
    new_password: Option<(String, String)>,
    viewer_error: Option<String>,
    notice: Option<String>,
    settings_error: Option<String>,
    autostart: bool,
    tray: Option<Tray>,
    window_hooked: bool,
    /// Confirming that this domain computer is personal.
    declaring: bool,
    /// A chat message being written.
    chat_draft: String,
}

pub(crate) struct Address {
    pub(crate) ip: Ipv4Addr,
    text: String,
    adapter: String,
    /// VM, container and WSL adapters: rarely what a viewer should use.
    virtual_adapter: bool,
}

/// Local IPv4 addresses a viewer could use, real network adapters first.
pub(crate) fn local_addresses(port: u16) -> Vec<Address> {
    const VIRTUAL: &[&str] = &[
        "vethernet",
        "vmware",
        "virtualbox",
        "hyper-v",
        "wsl",
        "docker",
        "loopback",
        "npcap",
        "bluetooth",
    ];
    let mut found: Vec<Address> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| {
            let IpAddr::V4(ip) = i.ip() else {
                return None;
            };
            let lower = i.name.to_lowercase();
            Some(Address {
                ip,
                text: if port == tidedesk_core::DEFAULT_PORT {
                    ip.to_string()
                } else {
                    format!("{ip}:{port}")
                },
                virtual_adapter: VIRTUAL.iter().any(|v| lower.contains(v)),
                adapter: i.name,
            })
        })
        .collect();
    found.sort_by(|a, b| (a.virtual_adapter, &a.text).cmp(&(b.virtual_adapter, &b.text)));
    found.dedup_by(|a, b| a.text == b.text);
    found
}

/// Where this computer stands as an unlicensed company computer, looked
/// up again every few seconds rather than on every frame.
type CompanyCache = Option<(Instant, Option<String>)>;
static COMPANY_CACHE: std::sync::Mutex<CompanyCache> = std::sync::Mutex::new(None);

/// What the Share tab says about this computer as a company computer: its
/// hours, or the notice while they do not apply yet.
fn company_line() -> Option<String> {
    use tidedesk_core::company;
    let mut cache = COMPANY_CACHE.lock().unwrap();
    match &*cache {
        Some((at, line)) if at.elapsed() < Duration::from_secs(5) => line.clone(),
        _ => {
            let name = company::management().name();
            let line = match company::allowance() {
                Some(allowance) => Some(allowance.describe(&name)),
                None if company::needs_licence() => Some(company::notice(&name)),
                None => None,
            };
            *cache = Some((Instant::now(), line.clone()));
            line
        }
    }
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

fn copy_button(ui: &mut egui::Ui, text: &str) {
    if ui.small_button("Copy").clicked() {
        ui.ctx().copy_text(text.to_string());
    }
}

pub fn run(info: HostInfo) -> Result<()> {
    let mut config = SoftwareBackendAppConfiguration::new();
    config.viewport_builder = egui::ViewportBuilder::default()
        .with_title(WINDOW_TITLE)
        .with_icon(icon::egui_icon())
        .with_inner_size([440.0, 540.0])
        .with_min_inner_size([380.0, 440.0])
        .with_taskbar(info.config.show_in_taskbar)
        .with_visible(!info.start_hidden);
    let mut app = Some(HostApp::new(info, WINDOW_TITLE));
    egui_software_backend::run_app_with_software_backend(config, move |ctx| {
        let mut app = app.take().expect("the host window is created once");
        app.attach(ctx);
        app
    })
    .map_err(|e| anyhow!("cannot open the host window: {e}"))
}

impl HostApp {
    /// The host's side of a window titled `title`; sharing has started
    /// already ([`crate::start`]).
    pub fn new(info: HostInfo, title: &'static str) -> Self {
        let displays = capture::list_displays().unwrap_or_default();
        let addresses = local_addresses(info.port);
        Self {
            info,
            title,
            tab: Tab::Status,
            displays,
            addresses,
            show_all_addresses: false,
            viewer_text: String::new(),
            new_password: None,
            viewer_error: None,
            notice: None,
            settings_error: None,
            autostart: platform::autostart_enabled(),
            declaring: false,
            chat_draft: String::new(),
            tray: None,
            window_hooked: false,
        }
    }

    /// Once the window's event loop runs: repaints when the host changes and
    /// adds the tray icon.
    pub fn attach(&mut self, ctx: egui::Context) {
        let state = self.info.state.clone();
        *state.on_change.lock().unwrap() = Some(Box::new(move || ctx.request_repaint()));
        match Tray::new(state, self.title) {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => tracing::warn!("no tray icon: {e:#}"),
        }
    }

    /// Once per frame, before drawing: hides on close when a tray icon can
    /// bring the window back, and mirrors the state into the tray.
    pub fn frame(&mut self) {
        if !self.window_hooked && self.tray.is_some() {
            // Only hide on close when there is a tray icon to come back from.
            platform::hide_on_close(self.title);
            self.window_hooked = true;
        }
        if let Some(tray) = &mut self.tray {
            tray.sync(&self.info.state);
        }
    }
}

impl HostApp {
    fn save(&mut self) {
        self.settings_error = self
            .info
            .config
            .save()
            .err()
            .map(|e| format!("Could not save settings: {e:#}"));
    }

    /// The viewers this host trusts, each with a way to stop trusting it.
    fn trusted_ui(&mut self, ui: &mut egui::Ui, state: &HostState) {
        let mut trusted = state.trusted.lock().unwrap();
        if trusted.list().is_empty() {
            return;
        }
        ui.label("Trusted viewers: they connect without the access code");
        let mut remove = None;
        for viewer in trusted.list() {
            ui.horizontal(|ui| {
                let short: String = viewer.fingerprint.chars().take(8).collect();
                ui.label(format!(
                    "{} ({short}…, since {})",
                    viewer.name, viewer.since
                ));
                if ui.small_button("Remove").clicked() {
                    remove = Some(viewer.fingerprint.clone());
                }
            });
        }
        if let Some(fingerprint) = remove {
            trusted.remove(&fingerprint);
            if let Err(e) = trusted.save() {
                self.notice = Some(format!("{e:#}"));
            }
        }
    }

    /// A password for the owner's own computers: viewers that have
    /// connected before use it instead of the access code.
    fn password_ui(&mut self, ui: &mut egui::Ui, state: &HostState) {
        ui.add_space(4.0);
        let set = state.password.lock().unwrap().is_some();
        match &mut self.new_password {
            None => {
                ui.horizontal(|ui| {
                    if set {
                        ui.label("Password: set");
                        if ui.small_button("Change").clicked() {
                            self.new_password = Some(Default::default());
                        }
                        if ui.small_button("Remove").clicked() {
                            match crate::saved_password::remove() {
                                Ok(()) => *state.password.lock().unwrap() = None,
                                Err(e) => self.notice = Some(format!("{e:#}")),
                            }
                        }
                    } else {
                        ui.label("Password: none");
                        if ui.small_button("Set a password").clicked() {
                            self.new_password = Some(Default::default());
                        }
                    }
                });
                ui.small(
                    "For your own computers: a viewer that has connected here before can use \
                     the password instead of the access code.",
                );
            }
            Some((first, again)) => {
                ui.label("New password");
                ui.add(egui::TextEdit::singleline(first).password(true));
                ui.label("The same again");
                ui.add(egui::TextEdit::singleline(again).password(true));
                let problem = tidedesk_core::password::problem(first)
                    .or((!again.is_empty() && first != again).then_some("The two differ."));
                if let Some(problem) = problem.filter(|_| !first.is_empty()) {
                    ui.colored_label(ERROR_RED, problem);
                }
                let ready = problem.is_none() && first == again;
                let mut close = false;
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("Save")).clicked() {
                        match crate::saved_password::save(first, &self.info.fingerprint) {
                            Ok(key) => {
                                *state.password.lock().unwrap() = Some(key);
                                close = true;
                            }
                            Err(e) => self.notice = Some(format!("{e:#}")),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                if close {
                    self.new_password = None;
                }
            }
        }
    }

    /// Access code, addresses, device ID and who is connected.
    pub fn status_tab(&mut self, ui: &mut egui::Ui) {
        let state = self.info.state.clone();

        let viewer = state.viewer.lock().unwrap().clone();
        let accepting = state.accepting.load(Ordering::SeqCst);
        ui.horizontal(|ui| match &viewer {
            Some(v) => {
                status_dot(ui, Color32::from_rgb(40, 180, 90));
                ui.label(format!("Connected: {} ({})", v.name, v.address.ip()));
                if ui.button("Disconnect").clicked() {
                    v.connection.close(2u32.into(), b"disconnected by host");
                }
                // An invitation: this viewer comes back without the code.
                if let Some(fingerprint) = &v.fingerprint {
                    let mut trusted = state.trusted.lock().unwrap();
                    if trusted.trusts(fingerprint) {
                        ui.label("Trusted");
                    } else if ui
                        .button("Trust this viewer")
                        .on_hover_text(
                            "It can connect again without the access code, until you remove \
                             it below.",
                        )
                        .clicked()
                    {
                        trusted.add(fingerprint, &v.name, &crate::trusted::today());
                        if let Err(e) = trusted.save() {
                            self.notice = Some(format!("{e:#}"));
                        }
                    }
                }
            }
            None if accepting => {
                status_dot(ui, Color32::from_rgb(60, 140, 230));
                ui.label("Waiting for a viewer");
            }
            None => {
                status_dot(ui, Color32::GRAY);
                ui.label("Paused: new viewers are refused");
            }
        });
        let mut accept = accepting;
        if ui.checkbox(&mut accept, "Accept new connections").changed() {
            state.accepting.store(accept, Ordering::SeqCst);
        }
        if let Some(line) = company_line() {
            let management = tidedesk_core::company::management();
            ui.colored_label(ui.visuals().warn_fg_color, line)
                .on_hover_text(
                    "Computers managed by an organisation need a TideDesk licence. Without one: \
                 14 days of trial, then 8 hours a month. Add a licence under About.",
                );
            if management.may_declare() && !self.declaring {
                self.declaring = ui
                    .small_button("This computer is mine…")
                    .on_hover_text("For a home lab that runs its own domain.")
                    .clicked();
            }
        }
        if self.declaring {
            ui.label(tidedesk_core::company::DECLARATION);
            ui.horizontal(|ui| {
                if ui.button("I declare this").clicked() {
                    if let Err(e) = tidedesk_core::company::declare() {
                        self.notice = Some(format!("{e:#}"));
                    }
                    *COMPANY_CACHE.lock().unwrap() = None;
                    self.declaring = false;
                }
                if ui.button("Cancel").clicked() {
                    self.declaring = false;
                }
            });
        } else if tidedesk_core::company::management().may_declare()
            && let Some(declared) = tidedesk_core::company::declaration()
        {
            ui.horizontal(|ui| {
                ui.small(declared.describe());
                if ui.small_button("Withdraw").clicked() {
                    if let Err(e) = tidedesk_core::company::withdraw() {
                        self.notice = Some(format!("{e:#}"));
                    }
                    *COMPANY_CACHE.lock().unwrap() = None;
                }
            });
        }
        if viewer.as_ref().is_some_and(|v| v.files)
            && ui
                .button("Send files to the viewer...")
                .on_hover_text("They are saved in Downloads\\TideDesk on the viewer's computer.")
                .clicked()
        {
            // To this session only: if it ends while the picker is open,
            // nothing goes to whoever connects next.
            let outgoing = state.outgoing.lock().unwrap().clone();
            platform::pick_files("Send files to the viewer", move |paths| {
                if let Some(outgoing) = outgoing {
                    for path in paths {
                        let _ = outgoing.send(path);
                    }
                }
            });
        }
        if let Some(note) = state.files_note.lock().unwrap().as_ref() {
            ui.small(note);
        }
        if viewer.as_ref().is_some_and(|v| v.files) {
            self.chat(ui, &state);
        }
        ui.separator();

        ui.label("Access code");
        let code = state.codes.lock().unwrap().current().to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&code).monospace().size(26.0).strong());
            ui.vertical(|ui| {
                copy_button(ui, &code);
                if ui.small_button("New code").clicked()
                    && let Err(e) = state.renew_code(false)
                {
                    self.notice = Some(format!("Could not save a new code: {e:#}"));
                }
            });
        });
        let mut after_session = state.new_code_after_session.load(Ordering::SeqCst);
        if ui
            .checkbox(&mut after_session, "New code after each session")
            .on_hover_text(
                "When a session ends, the code it used is replaced. It still works for five \
                 minutes, so a dropped connection comes straight back.",
            )
            .changed()
        {
            state
                .new_code_after_session
                .store(after_session, Ordering::SeqCst);
            self.info.config.new_code_after_session = after_session;
            if let Err(e) = self.info.config.save() {
                self.notice = Some(format!("Could not save: {e:#}"));
            }
        }
        if let Some(note) = state.code_note.lock().unwrap().as_ref() {
            ui.small(note);
        }
        if let Some(n) = &self.notice {
            ui.small(n);
        }
        // Where the "install this and read me the code" scam happens: say it here.
        ui.small(
            RichText::new(
                "Give this code only to someone you know and trust. If a stranger asked you to \
                 install TideDesk or to read out this code, stop: they may be trying to take \
                 control of your computer.",
            )
            .color(WARNING_AMBER),
        );
        self.password_ui(ui, &state);
        self.trusted_ui(ui, &state);
        // Who guessed wrong, for the person at the host to see.
        let blocked = state.throttle.lock().unwrap().blocked(Instant::now());
        for address in &blocked {
            ui.colored_label(ERROR_RED, blocked_line(address));
        }
        if !blocked.is_empty() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        ui.add_space(6.0);

        ui.label("This computer's addresses");
        if self.addresses.is_empty() {
            ui.small("No network connection found.");
        }
        let real = self.addresses.iter().filter(|a| !a.virtual_adapter).count();
        let hidden = self.addresses.len() - real;
        // With no real adapter, virtual ones are all there is to offer.
        let show_all = self.show_all_addresses || real == 0;
        for a in self
            .addresses
            .iter()
            .filter(|a| show_all || !a.virtual_adapter)
        {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&a.text).monospace());
                ui.small(RichText::new(&a.adapter).weak());
                copy_button(ui, &a.text);
            });
        }
        if hidden > 0 && real > 0 {
            let label = format!("Show virtual adapters ({hidden})");
            ui.checkbox(&mut self.show_all_addresses, RichText::new(label).small());
        }
        ui.add_space(6.0);

        ui.label("Internet address");
        self.internet_address(ui);
        ui.add_space(6.0);

        ui.label("Viewer on another network");
        self.expected_viewer(ui);
        ui.add_space(6.0);

        ui.label("Device ID");
        let device_id = self.info.identity.device_id.to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&device_id).monospace());
            copy_button(ui, &device_id);
        });
        ui.small(internet::describe_rendezvous(&self.info.agent.rendezvous()));
        let registers = self.info.agent.rendezvous() != RendezvousStatus::Off;
        if let Some(line) = internet::describe_lan_discovery(
            self.info.config.lan_discovery,
            self.info.port,
            registers,
        ) {
            ui.small(line);
        }
        ui.add_space(6.0);

        ui.label("Fingerprint");
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&self.info.fingerprint).monospace().small());
            copy_button(ui, &self.info.fingerprint);
        });
        ui.small("Viewers see this on first connect; it should match.");
    }

    fn internet_address(&self, ui: &mut egui::Ui) {
        match self.info.agent.public() {
            PublicStatus::Disabled => {
                ui.small("Not looked up. Turn it on under Settings, Internet.");
            }
            PublicStatus::Discovering => {
                ui.small("Looking it up…");
            }
            PublicStatus::Unavailable(reason) => {
                ui.small(format!("Unavailable: {reason}"));
            }
            PublicStatus::Ready(public) if public.nat == NatKind::Symmetric => {
                ui.small(
                    "Unavailable: this network uses a symmetric NAT, so direct internet \
                     connections will not work from here. A VPN or port forwarding still works.",
                );
            }
            PublicStatus::Ready(public) => {
                let text = public.addr.to_string();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&text).monospace());
                    copy_button(ui, &text);
                });
                ui.small(RichText::new(format!("via {}, NAT: {}", public.via, public.nat)).weak());
                ui.small(
                    "A viewer on another network connects to this address with \
                     \"Over the internet\" ticked, once you open a path to it below.",
                );
            }
        }
    }

    fn expected_viewer(&mut self, ui: &mut egui::Ui) {
        let state = self.info.state.clone();
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.viewer_text)
                    .hint_text("its internet address")
                    .desired_width(180.0),
            );
            if field.changed() {
                self.viewer_error = None;
            }
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Open").clicked() || entered {
                let own = match self.info.agent.public() {
                    PublicStatus::Ready(public) => Some(public.addr.ip()),
                    _ => None,
                };
                match internet::parse_expected_viewer(&self.viewer_text, own) {
                    Ok(typed) => {
                        self.viewer_error = None;
                        internet::open_path(&state, &self.info.agent, &self.info.runtime, typed);
                    }
                    Err(e) => self.viewer_error = Some(e),
                }
            }
        });
        let expected = state.expected_viewer.lock().unwrap().clone();
        if let Some(error) = &self.viewer_error {
            ui.colored_label(ERROR_RED, error);
        } else if let Some(expected) = expected {
            let now = Instant::now();
            ui.small(expected.describe(now));
            match expected.state {
                PathState::Opening { .. } => ui.ctx().request_repaint_after(Duration::from_secs(1)),
                // Show when the keepalives stop.
                PathState::Open { until, .. } if until > now => {
                    ui.ctx().request_repaint_after(until - now)
                }
                _ => {}
            }
        } else {
            ui.small(
                "Type the internet address the viewer shows and press Open, then connect \
                 from the viewer within two minutes.",
            );
        }
    }

    /// The chat with the viewer: what was written, and a box to write.
    fn chat(&mut self, ui: &mut egui::Ui, state: &HostState) {
        egui::CollapsingHeader::new("Chat with the viewer")
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for (theirs, text) in state.chat.lock().unwrap().iter() {
                            let who = if *theirs { "Viewer" } else { "You" };
                            ui.label(RichText::new(who).small().strong());
                            ui.label(text);
                        }
                    });
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.chat_draft)
                            .char_limit(tidedesk_core::chat::MAX_CHARS)
                            .hint_text("Write to the viewer"),
                    );
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (enter || ui.button("Send").clicked())
                        && let Some(text) = tidedesk_core::chat::clean(&self.chat_draft)
                        && let Some(out) = state.chat_out.lock().unwrap().as_ref()
                        && out.send(text.clone()).is_ok()
                    {
                        state.chat.lock().unwrap().push((false, text));
                        self.chat_draft.clear();
                        edit.request_focus();
                    }
                });
            });
    }

    /// The sharing settings; changes apply at once and are saved.
    pub fn settings_tab(&mut self, ui: &mut egui::Ui) {
        let title = self.title;
        let state = self.info.state.clone();
        let before = self.info.config.clone();
        let cfg = &mut self.info.config;

        ui.label(RichText::new("Sharing").strong());
        ui.small("Applies to the next connection.");
        egui::Grid::new("sharing")
            .num_columns(2)
            .spacing([12.0, 8.0])
            .show(ui, |ui| {
                ui.label("Screen");
                let current = self
                    .displays
                    .iter()
                    .find(|d| d.index == cfg.display)
                    .map(display_label)
                    .unwrap_or_else(|| format!("Display {}", cfg.display + 1));
                egui::ComboBox::from_id_salt("display")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for d in &self.displays {
                            ui.selectable_value(&mut cfg.display, d.index, display_label(d));
                        }
                    });
                ui.end_row();

                ui.label("Frame rate");
                ui.add(egui::Slider::new(&mut cfg.fps, HostConfig::FPS_RANGE).suffix(" fps"));
                ui.end_row();

                ui.label("Quality");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut cfg.automatic_bitrate, "Automatic")
                        .on_hover_text(
                            "Sets the bitrate by the screen's size: about 6 Mbit/s at \
                             1920x1080, 12 Mbit/s at 2560x1600, up to 20 Mbit/s.",
                        );
                    if !cfg.automatic_bitrate {
                        ui.add(
                            egui::Slider::new(&mut cfg.bitrate_kbps, HostConfig::BITRATE_RANGE)
                                .suffix(" kbit/s")
                                .logarithmic(true),
                        );
                    } else if let Some(d) = self.displays.iter().find(|d| d.index == cfg.display) {
                        let size = ((d.rect.width as usize) & !1, (d.rect.height as usize) & !1);
                        let bps = crate::video::automatic_bitrate(size);
                        ui.label(format!(
                            "{:.1} Mbit/s for this display",
                            f64::from(bps) / 1e6
                        ));
                    }
                });
                ui.end_row();
            });
        ui.checkbox(&mut cfg.share_audio, "Share sound");
        ui.add_space(8.0);
        ui.label(RichText::new("Live session permissions").strong());
        ui.checkbox(&mut cfg.allow_clipboard, "Allow text clipboard sharing");
        ui.checkbox(&mut cfg.allow_mouse, "Allow viewer mouse control");
        ui.checkbox(&mut cfg.allow_files, "Allow files from the viewer")
            .on_hover_text(
                "Files dropped on the viewer's window are saved in Downloads\\TideDesk.",
            );
        ui.small("Applies immediately. Clipboard also needs to be enabled in Viewer Settings.");
        ui.add_space(8.0);
        if crate::session_log::on()
            && let Ok(log) = crate::session_log::path()
        {
            ui.label(RichText::new("Session log").strong());
            ui.small(format!(
                "Every session is recorded in {}: who, from where, when, and why it ended.",
                log.display()
            ));
            if log.exists() && ui.button("Open the session log").clicked() {
                platform::open_link(&log.to_string_lossy());
            }
            ui.add_space(8.0);
        }

        ui.label(RichText::new("Window").strong());
        ui.checkbox(&mut cfg.show_in_taskbar, "Show in the taskbar")
            .on_hover_text("When off, TideDesk Host lives in the notification area (tray) only.");
        ui.checkbox(&mut cfg.start_in_tray, "Start hidden in the tray");
        let mut autostart = self.autostart;
        if ui
            .checkbox(&mut autostart, "Start when I sign in to Windows")
            .changed()
        {
            match platform::set_autostart(autostart) {
                Ok(()) => self.autostart = autostart,
                Err(e) => self.settings_error = Some(format!("Could not change start-up: {e:#}")),
            }
        }
        ui.small("Closing the window keeps TideDesk running in the tray; quit from the tray menu.");
        ui.add_space(8.0);

        ui.label(RichText::new("Network").strong());
        ui.horizontal(|ui| {
            ui.label("UDP port");
            ui.add(egui::DragValue::new(&mut cfg.port).range(1024..=65535));
        });
        if cfg.port != self.info.port {
            ui.small("The new port is used after TideDesk Host restarts.");
        }
        ui.checkbox(
            &mut cfg.lan_discovery,
            "Reachable by device ID on this network",
        )
        .on_hover_text(
            "Answers viewers on this local network that look for this computer's device \
                 ID, so they connect directly even without the internet. Only computers on \
                 this network get an answer.",
        );
        ui.add_space(8.0);

        ui.label(RichText::new("Internet").strong());
        ui.checkbox(
            &mut cfg.rendezvous,
            "Reachable by device ID from other networks",
        )
        .on_hover_text(
            "Registers this computer's device ID and public address with TideDesk's \
             connection service, which introduces viewers and never carries a session.",
        );
        ui.checkbox(
            &mut cfg.discover_public_address,
            "Look up this computer's internet address (STUN)",
        )
        .on_hover_text(
            "Asks public STUN servers which address and port your router gives \
             TideDesk. They see this computer's public IP address and a 20-byte \
             request, nothing else.",
        );

        if *cfg != before {
            {
                let mut video = state.video.lock().unwrap();
                video.display = cfg.display;
                video.fps = cfg.fps;
                video.bitrate_bps = cfg.bitrate_bps();
            }
            state.audio.store(cfg.share_audio, Ordering::SeqCst);
            state.clipboard.store(cfg.allow_clipboard, Ordering::SeqCst);
            state.mouse.store(cfg.allow_mouse, Ordering::SeqCst);
            state.files.store(cfg.allow_files, Ordering::SeqCst);
            if cfg.show_in_taskbar != before.show_in_taskbar {
                platform::set_taskbar_button(title, cfg.show_in_taskbar);
            }
            if cfg.rendezvous != before.rendezvous
                || cfg.rendezvous_server != before.rendezvous_server
            {
                match cfg.rendezvous_service() {
                    Some(service) => {
                        let code = state.codes.lock().unwrap().current().to_string();
                        let credentials = crate::registration_credentials(
                            &self.info.identity,
                            &code,
                            self.info.port,
                        );
                        self.info
                            .agent
                            .start_rendezvous(service.to_string(), credentials);
                        *state.registration.lock().unwrap() = Some(service.to_string());
                    }
                    None => {
                        self.info.agent.stop_rendezvous();
                        *state.registration.lock().unwrap() = None;
                    }
                }
            }
            if cfg.lan_discovery != before.lan_discovery {
                if cfg.lan_discovery {
                    let device_id = self.info.identity.device_id;
                    self.info.agent.start_lan_discovery(device_id);
                } else {
                    self.info.agent.stop_lan_discovery();
                }
            }
            if cfg.discover_public_address != before.discover_public_address
                || cfg.stun_servers != before.stun_servers
            {
                if cfg.discover_public_address {
                    let servers = cfg.effective_stun_servers();
                    self.info.agent.start_refresh(servers, STUN_REFRESH);
                } else {
                    self.info.agent.stop_refresh();
                }
            }
            self.save();
        }
        if let Some(e) = &self.settings_error {
            ui.colored_label(ERROR_RED, e);
        }
    }
}

impl egui_software_backend::App for HostApp {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut SoftwareBackend) {
        self.frame();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading(format!("TideDesk | {}", self.info.state.host_name));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Status, "Status");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Status => self.status_tab(ui),
                Tab::Settings => self.settings_tab(ui),
            });
        });
    }
}

fn display_label(d: &DisplayInfo) -> String {
    format!(
        "Display {}: {}×{}{}",
        d.index + 1,
        d.rect.width,
        d.rect.height,
        if d.primary { " (main)" } else { "" }
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_blocked_address_is_named_with_its_wait() {
        use std::time::Duration;
        use tidedesk_core::auth::Blocked;
        let blocked = |s| Blocked {
            address: [203, 0, 113, 9].into(),
            failures: 7,
            for_another: Duration::from_secs(s),
        };
        assert_eq!(
            super::blocked_line(&blocked(8)),
            "7 wrong codes from 203.0.113.9: refused for 8 s more"
        );
        assert_eq!(
            super::blocked_line(&blocked(3599)),
            "7 wrong codes from 203.0.113.9: refused for 60 min more"
        );
    }
}
