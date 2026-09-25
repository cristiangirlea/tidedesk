//! The host window: status, access code and addresses, plus a settings tab.
//!
//! Drawn on the CPU (egui + a software rasteriser) rather than through OpenGL
//! or Direct3D: a GPU context alone costs 150-500 MB on some drivers, while this
//! window needs a few MB. egui only repaints on input or when the server reports
//! a change, so an open window costs nothing while nothing happens.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use egui::{Color32, RichText};
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_core::nat::signal::Credentials;
use tidedesk_core::nat::signal::DEFAULT_RENDEZVOUS;
use tidedesk_core::nat::stun::{DEFAULT_STUN_SERVERS, STUN_REFRESH};
use tidedesk_core::nat::{Agent, NatKind, PublicStatus};

use crate::capture::{self, DisplayInfo};
use crate::config::{HostConfig, parse_stun_servers};
use crate::internet::{self, PathState};
use crate::session::HostState;
use crate::tray::Tray;
use crate::{icon, platform};

/// Also used to find the native window for tray and taskbar handling.
pub const WINDOW_TITLE: &str = "TideDesk Host";

const ERROR_RED: Color32 = Color32::from_rgb(220, 60, 50);

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
    /// The STUN server list as typed; applied when the field loses focus.
    stun_text: String,
    /// The rendezvous service as typed; applied when the field loses focus.
    rendezvous_text: String,
    /// A viewer's internet address as typed, and why it was not accepted.
    viewer_text: String,
    viewer_error: Option<String>,
    notice: Option<String>,
    settings_error: Option<String>,
    autostart: bool,
    tray: Option<Tray>,
    window_hooked: bool,
}

struct Address {
    text: String,
    adapter: String,
    /// VM, container and WSL adapters: rarely what a viewer should use.
    virtual_adapter: bool,
}

/// Local IPv4 addresses a viewer could use, real network adapters first.
fn local_addresses(port: u16) -> Vec<Address> {
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
        .filter(|i| !i.is_loopback() && i.ip().is_ipv4())
        .map(|i| {
            let ip = i.ip();
            let lower = i.name.to_lowercase();
            Address {
                text: if port == tidedesk_core::DEFAULT_PORT {
                    ip.to_string()
                } else {
                    format!("{ip}:{port}")
                },
                virtual_adapter: VIRTUAL.iter().any(|v| lower.contains(v)),
                adapter: i.name,
            }
        })
        .collect();
    found.sort_by(|a, b| (a.virtual_adapter, &a.text).cmp(&(b.virtual_adapter, &b.text)));
    found.dedup_by(|a, b| a.text == b.text);
    found
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
        let stun_text = info.config.stun_servers.join(", ");
        let rendezvous_text = info.config.rendezvous_server.clone();
        Self {
            info,
            title,
            tab: Tab::Status,
            displays,
            addresses,
            show_all_addresses: false,
            stun_text,
            rendezvous_text,
            viewer_text: String::new(),
            viewer_error: None,
            notice: None,
            settings_error: None,
            autostart: platform::autostart_enabled(),
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
            }
            None if accepting => {
                status_dot(ui, Color32::from_rgb(60, 140, 230));
                ui.label("Waiting for a viewer");
            }
            None => {
                status_dot(ui, Color32::GRAY);
                ui.label("Paused — new viewers are refused");
            }
        });
        let mut accept = accepting;
        if ui.checkbox(&mut accept, "Accept new connections").changed() {
            state.accepting.store(accept, Ordering::SeqCst);
        }
        ui.separator();

        ui.label("Access code");
        let code = state.code.lock().unwrap().clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&code).monospace().size(26.0).strong());
            ui.vertical(|ui| {
                copy_button(ui, &code);
                if ui.small_button("New code").clicked() {
                    match crate::load_code(true) {
                        Ok(new) => {
                            *state.code.lock().unwrap() = new;
                            self.notice =
                                Some("New code saved. The old one no longer works.".into());
                        }
                        Err(e) => self.notice = Some(format!("Could not save a new code: {e:#}")),
                    }
                }
            });
        });
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
            .color(Color32::from_rgb(180, 110, 0)),
        );
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
                ui.add(
                    egui::Slider::new(&mut cfg.bitrate_kbps, HostConfig::BITRATE_RANGE)
                        .suffix(" kbit/s")
                        .logarithmic(true),
                );
                ui.end_row();
            });
        ui.checkbox(&mut cfg.share_audio, "Share sound");
        ui.add_space(8.0);
        ui.label(RichText::new("Live session permissions").strong());
        ui.checkbox(&mut cfg.allow_clipboard, "Allow text clipboard sharing");
        ui.checkbox(&mut cfg.allow_mouse, "Allow viewer mouse control");
        ui.small("Applies immediately. Clipboard also needs to be enabled in Viewer Settings.");
        ui.add_space(8.0);

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
        ui.add_space(8.0);

        ui.label(RichText::new("Internet").strong());
        ui.checkbox(
            &mut cfg.rendezvous,
            "Reachable by device ID from other networks",
        )
        .on_hover_text(
            "Registers this computer's device ID and public address with TideDesk's \
             connection service (or the one under Advanced), which introduces viewers \
             and never carries a session.",
        );
        egui::CollapsingHeader::new("Advanced")
            .id_salt("internet-advanced")
            .show(ui, |ui| {
                ui.checkbox(
                    &mut cfg.discover_public_address,
                    "Look up this computer's internet address (STUN)",
                )
                .on_hover_text(
                    "Asks public STUN servers which address and port your router gives \
                     TideDesk. They see this computer's public IP address and a 20-byte \
                     request, nothing else.",
                );
                ui.horizontal(|ui| {
                    ui.label("STUN servers");
                    let field = egui::TextEdit::singleline(&mut self.stun_text)
                        .hint_text(DEFAULT_STUN_SERVERS.join(", "));
                    if ui
                        .add_enabled(cfg.discover_public_address, field)
                        .lost_focus()
                    {
                        cfg.stun_servers = parse_stun_servers(&self.stun_text);
                        self.stun_text = cfg.stun_servers.join(", ");
                    }
                });
                ui.small("host:port, separated by commas. Leave empty for the defaults.");
                ui.horizontal(|ui| {
                    ui.label("Connection service");
                    let field = egui::TextEdit::singleline(&mut self.rendezvous_text)
                        .hint_text(DEFAULT_RENDEZVOUS);
                    if ui.add_enabled(cfg.rendezvous, field).lost_focus() {
                        cfg.rendezvous_server = self.rendezvous_text.trim().to_string();
                        self.rendezvous_text = cfg.rendezvous_server.clone();
                    }
                });
                ui.small("host:port. Leave empty for TideDesk's own service.");
            });

        if *cfg != before {
            {
                let mut video = state.video.lock().unwrap();
                video.display = cfg.display;
                video.fps = cfg.fps;
                video.bitrate_bps = cfg.bitrate_kbps * 1000;
            }
            state.audio.store(cfg.share_audio, Ordering::SeqCst);
            state.clipboard.store(cfg.allow_clipboard, Ordering::SeqCst);
            state.mouse.store(cfg.allow_mouse, Ordering::SeqCst);
            if cfg.show_in_taskbar != before.show_in_taskbar {
                platform::set_taskbar_button(title, cfg.show_in_taskbar);
            }
            if cfg.rendezvous != before.rendezvous
                || cfg.rendezvous_server != before.rendezvous_server
            {
                match cfg.rendezvous_service() {
                    Some(service) => {
                        let credentials = self.info.identity.clone();
                        self.info
                            .agent
                            .start_rendezvous(service.to_string(), credentials);
                    }
                    None => self.info.agent.stop_rendezvous(),
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
            ui.heading(format!("TideDesk — {}", self.info.state.host_name));
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
        "Display {} — {}×{}{}",
        d.index + 1,
        d.rect.width,
        d.rect.height,
        if d.primary { " (main)" } else { "" }
    )
}
