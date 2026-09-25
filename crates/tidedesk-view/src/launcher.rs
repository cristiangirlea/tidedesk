//! The viewer's main window: quick connect plus a list of saved computers.
//!
//! The remote-screen window runs in a child process: winit allows one event
//! loop per process, and a separate process also keeps this window responsive
//! and able to report why a session ended.
//!
//! A computer on the local network (or a VPN) is probed here before the
//! session starts. For one reached over the internet everything happens in
//! the session process, which owns the punched path; it reports its steps
//! and asks about the host's identity through the lines in [`crate::child`].

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use egui::{Color32, RichText};
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_core::identity::{KnownHosts, PinStatus};
use tidedesk_core::paths;

use crate::child::{self, ChildLine};
use crate::computers::{AddressBook, Computer};
use crate::connect::{self, Probe};
use crate::icon;

const ERROR: Color32 = Color32::from_rgb(220, 60, 50);

const INTERNET_OPTION: &str = "Over the internet (the host shows its internet address)";

/// A connection about to be made.
#[derive(Clone)]
struct Target {
    address: String,
    code: String,
    sound: bool,
    internet: bool,
}

/// How a session process reaches its host.
enum SessionRoute {
    /// Probed by the launcher already.
    Direct,
    Internet,
    /// By device ID, through this rendezvous service.
    Rendezvous(String),
}

/// Writes answers to a session process that is still connecting.
#[derive(Clone)]
struct SessionControl(Arc<Mutex<Option<ChildStdin>>>);

impl SessionControl {
    fn send(&self, answer: &str) {
        if let Some(stdin) = self.0.lock().unwrap().as_mut() {
            let _ = writeln!(stdin, "{answer}").and_then(|()| stdin.flush());
        }
    }
}

enum Phase {
    Idle,
    Checking(Target),
    /// The host's identity needs the user's approval.
    Confirm(Probe, Target),
    /// A session process is opening an internet path and connecting.
    Opening {
        host: String,
        status: String,
        viewer_address: Option<SocketAddr>,
        control: SessionControl,
    },
    /// A connecting session process waits for the user to trust the host.
    ConfirmSession {
        host: String,
        probe: Probe,
        control: SessionControl,
    },
    InSession(String),
}

/// Results handed back from worker threads.
enum Update {
    Probed(Result<Probe>),
    Child(ChildLine),
    SessionEnded {
        host: String,
        outcome: Result<(), String>,
    },
}

/// Add/edit form for a saved computer.
struct Editor {
    index: Option<usize>,
    name: String,
    address: String,
    code: String,
    remember_code: bool,
    had_code: bool,
    sound: bool,
    internet: bool,
    error: Option<String>,
}

struct Launcher {
    // Quick connect.
    address: String,
    code: String,
    sound: bool,
    internet: bool,
    focus_code: bool,

    book: AddressBook,
    recent: Vec<String>,
    editor: Option<Editor>,
    pending_delete: Option<usize>,
    filter: String,

    phase: Phase,
    message: Option<(bool, String)>, // (is_error, text)
    inbox: Arc<Mutex<Vec<Update>>>,
    show_settings: bool,
    settings_editor: crate::settings::Editor,
}

pub fn run() -> Result<()> {
    let mut config = SoftwareBackendAppConfiguration::new();
    config.viewport_builder = egui::ViewportBuilder::default()
        .with_title("TideDesk Viewer")
        .with_icon(icon::egui_icon())
        .with_inner_size([460.0, 580.0])
        .with_min_inner_size([380.0, 420.0]);
    egui_software_backend::run_app_with_software_backend(config, |_ctx| Launcher {
        address: String::new(),
        code: String::new(),
        sound: true,
        internet: false,
        focus_code: false,
        book: AddressBook::load(),
        recent: load_recent(),
        editor: None,
        pending_delete: None,
        filter: String::new(),
        phase: Phase::Idle,
        message: None,
        inbox: Arc::default(),
        show_settings: false,
        settings_editor: crate::settings::Editor::default(),
    })
    .map_err(|e| anyhow!("cannot open the viewer window: {e}"))
}

fn load_recent() -> Vec<String> {
    paths::config_dir()
        .and_then(|d| KnownHosts::load(&d))
        .map(|k| k.addresses().map(str::to_string).collect())
        .unwrap_or_default()
}

impl Launcher {
    fn post(inbox: &Arc<Mutex<Vec<Update>>>, ctx: &egui::Context, update: Update) {
        inbox.lock().unwrap().push(update);
        ctx.request_repaint();
    }

    fn connect(&mut self, ctx: &egui::Context, target: Target) {
        if target.code.trim().is_empty() {
            // No saved code: collect it in the quick-connect form.
            self.address = target.address;
            self.sound = target.sound;
            self.internet = target.internet;
            self.code.clear();
            self.focus_code = true;
            self.message = Some((
                false,
                "Enter the access code shown on that computer.".into(),
            ));
            return;
        }
        self.message = None;
        let host = target.address.trim().to_string();
        // Internet and device-ID sessions open their path and probe the host
        // themselves, in the session process that owns the path.
        if let Some(id) = connect::parse_device_id(&host) {
            let saved = crate::settings::ViewerSettings::load()
                .map(|s| s.rendezvous_server)
                .unwrap_or_default();
            let route = SessionRoute::Rendezvous(crate::rendezvous_service(None, Some(&saved)));
            return self.start_session(ctx, id.to_string(), &target, route);
        }
        if target.internet {
            return match connect::parse_internet_host(&host) {
                Ok(addr) => {
                    self.start_session(ctx, addr.to_string(), &target, SessionRoute::Internet)
                }
                Err(e) => self.fail(format!("{e:#}")),
            };
        }
        self.phase = Phase::Checking(target);
        let (inbox, ctx) = (self.inbox.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| rt.block_on(connect::probe(&host)));
            Self::post(&inbox, &ctx, Update::Probed(result));
        });
    }

    fn start_session(
        &mut self,
        ctx: &egui::Context,
        host: String,
        target: &Target,
        route: SessionRoute,
    ) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return self.fail(format!("cannot locate viewer: {e}")),
        };
        let mut cmd = Command::new(exe);
        cmd.args(crate::self_prefix())
            .args(session_args(&host, target.sound, &route))
            // The code travels in the environment, not the (visible) command line.
            .env("TIDEDESK_CODE", target.code.trim())
            .env(child::LAUNCHER_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let opening = !matches!(route, SessionRoute::Direct);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return self.fail(format!("cannot start session: {e}")),
        };

        let control = SessionControl(Arc::new(Mutex::new(child.stdin.take())));
        self.message = None;
        if opening {
            self.phase = Phase::Opening {
                host: host.clone(),
                status: "Starting…".into(),
                viewer_address: None,
                control,
            };
        } else {
            // A direct session needs no answers: dropping `control` closes its stdin.
            self.phase = Phase::InSession(host.clone());
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        let (inbox, ctx2) = (self.inbox.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut lines = Vec::new();
            if let Some(stderr) = child.stderr.take() {
                // Bytes, not `lines()`: a line that is not UTF-8 must not stop
                // the reading, or the session would block on a full pipe.
                for raw in BufReader::new(stderr).split(b'\n') {
                    let Ok(raw) = raw else { break };
                    let line = child::parse(String::from_utf8_lossy(&raw).trim_end());
                    if matches!(
                        line,
                        ChildLine::Status(_)
                            | ChildLine::ViewerAddress(_)
                            | ChildLine::Fingerprint { .. }
                            | ChildLine::Connected(_)
                    ) {
                        Self::post(&inbox, &ctx2, Update::Child(line.clone()));
                    }
                    lines.push(line);
                }
            }
            let status = child.wait();
            // The session prints `error: …` or `disconnected: …` as its last word.
            let last_error = lines.iter().rev().find_map(|l| match l {
                ChildLine::Error(e) => Some(e.clone()),
                _ => None,
            });
            let last_disconnect = lines.iter().rev().find_map(|l| match l {
                ChildLine::Disconnected(reason) => Some(reason.clone()),
                _ => None,
            });
            let outcome = match status {
                Ok(s) if s.success() => Ok(()),
                _ => Err(last_error.unwrap_or_else(|| "the session ended unexpectedly".into())),
            };
            let outcome = match (outcome, last_disconnect) {
                (Ok(()), Some(reason)) if reason != "host ended the session" => Err(reason),
                (o, _) => o,
            };
            ctx2.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            Self::post(&inbox, &ctx2, Update::SessionEnded { host, outcome });
        });
    }

    /// Follows a session process that is opening an internet path.
    fn on_child_line(&mut self, ctx: &egui::Context, line: ChildLine) {
        let (host, control) = match &self.phase {
            Phase::Opening { host, control, .. } | Phase::ConfirmSession { host, control, .. } => {
                (host.clone(), control.clone())
            }
            _ => return,
        };
        match line {
            ChildLine::Status(text) => {
                if let Phase::Opening { status, .. } = &mut self.phase {
                    *status = text;
                }
            }
            ChildLine::ViewerAddress(me) => {
                if let Phase::Opening { viewer_address, .. } = &mut self.phase {
                    *viewer_address = Some(me);
                }
            }
            ChildLine::Fingerprint {
                address,
                fingerprint,
                status,
            } => {
                let probe = Probe {
                    address,
                    fingerprint,
                    status,
                };
                self.phase = Phase::ConfirmSession {
                    host,
                    probe,
                    control,
                };
            }
            ChildLine::Connected(_) => {
                // Dropping the control closes the session's stdin: no more questions.
                self.phase = Phase::InSession(host);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            _ => {}
        }
    }

    fn fail(&mut self, text: String) {
        self.phase = Phase::Idle;
        self.message = Some((true, text));
    }

    fn handle_updates(&mut self, ctx: &egui::Context) {
        let updates = std::mem::take(&mut *self.inbox.lock().unwrap());
        for update in updates {
            match update {
                Update::Probed(result) => {
                    let Phase::Checking(target) = std::mem::replace(&mut self.phase, Phase::Idle)
                    else {
                        continue;
                    };
                    match result {
                        Err(e) => self.fail(format!("{e:#}")),
                        Ok(probe) if probe.status == PinStatus::Trusted => {
                            self.start_session(ctx, probe.address, &target, SessionRoute::Direct)
                        }
                        Ok(probe) => self.phase = Phase::Confirm(probe, target),
                    }
                }
                Update::Child(line) => self.on_child_line(ctx, line),
                Update::SessionEnded { host, outcome } => {
                    self.phase = Phase::Idle;
                    self.message = Some(match outcome {
                        Ok(()) => (false, format!("Session with {host} ended.")),
                        Err(e) if e == child::CANCELLED => (false, "Cancelled.".into()),
                        Err(e) => (true, e),
                    });
                    self.recent = load_recent();
                }
            }
        }
    }

    fn trust(&mut self, ctx: &egui::Context, probe: &Probe, target: &Target) {
        let pinned = paths::config_dir()
            .and_then(|d| KnownHosts::load(&d))
            .and_then(|mut k| k.pin(&probe.address, &probe.fingerprint));
        match pinned {
            Ok(()) => self.start_session(ctx, probe.address.clone(), target, SessionRoute::Direct),
            Err(e) => self.fail(format!("cannot remember this host: {e:#}")),
        }
    }

    fn open_editor(&mut self, index: Option<usize>, address: &str) {
        let existing = index.and_then(|i| self.book.computers.get(i));
        self.editor = Some(Editor {
            index,
            name: existing.map(|c| c.name.clone()).unwrap_or_default(),
            address: existing
                .map(|c| c.address.clone())
                .unwrap_or_else(|| address.to_string()),
            code: String::new(),
            remember_code: existing.is_some_and(Computer::has_code),
            had_code: existing.is_some_and(Computer::has_code),
            sound: existing.is_none_or(|c| c.sound),
            internet: existing.is_some_and(|c| c.internet),
            error: None,
        });
    }

    fn editor_view(&mut self, ui: &mut egui::Ui) {
        let Some(ed) = &mut self.editor else { return };
        ui.heading(if ed.index.is_some() {
            "Edit computer"
        } else {
            "Add a computer"
        });
        ui.add_space(8.0);
        egui::Grid::new("editor")
            .num_columns(2)
            .spacing([8.0, 8.0])
            .show(ui, |ui| {
                ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut ed.name)
                        .hint_text("Office PC")
                        .desired_width(240.0),
                );
                ui.end_row();
                ui.label("Address");
                ui.add(
                    egui::TextEdit::singleline(&mut ed.address)
                        .hint_text(address_hint(ed.internet))
                        .desired_width(240.0),
                );
                ui.end_row();
            });
        ui.checkbox(&mut ed.internet, INTERNET_OPTION);
        ui.checkbox(&mut ed.remember_code, "Remember the access code");
        if ed.remember_code {
            let hint = if ed.had_code {
                "(unchanged)"
            } else {
                "XXXX-XXXX-XX"
            };
            ui.add(
                egui::TextEdit::singleline(&mut ed.code)
                    .hint_text(hint)
                    .desired_width(240.0),
            );
            ui.small("Encrypted so only your Windows account on this computer can read it.");
        }
        ui.checkbox(&mut ed.sound, "Play sound from this computer");
        if let Some(e) = &ed.error {
            ui.colored_label(ERROR, e);
        }
        ui.add_space(8.0);

        let mut close = false;
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                let name = ed.name.trim();
                let address = ed.address.trim();
                if let Some(problem) = address_problem(address, ed.internet) {
                    ed.error = Some(problem);
                    return;
                }
                let mut pc = ed
                    .index
                    .and_then(|i| self.book.computers.get(i).cloned())
                    .unwrap_or_else(|| Computer::new(String::new(), String::new(), true));
                pc.name = if name.is_empty() { address } else { name }.to_string();
                // Device IDs are kept in one spelling.
                pc.address = connect::parse_device_id(address)
                    .map_or_else(|| address.to_string(), |id| id.to_string());
                pc.sound = ed.sound;
                pc.internet = ed.internet;
                let code_result = if !ed.remember_code {
                    pc.set_code(None)
                } else if !ed.code.trim().is_empty() {
                    pc.set_code(Some(&ed.code))
                } else {
                    Ok(()) // keep whatever was saved
                };
                if let Err(e) = code_result {
                    ed.error = Some(format!("{e:#}"));
                    return;
                }
                match ed.index {
                    Some(i) => self.book.computers[i] = pc,
                    None => self.book.computers.push(pc),
                }
                self.book.computers.sort_by_key(|c| c.name.to_lowercase());
                match self.book.save() {
                    Ok(()) => close = true,
                    Err(e) => ed.error = Some(format!("{e:#}")),
                }
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
        if close {
            self.editor = None;
        }
    }

    fn main_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let busy = !matches!(self.phase, Phase::Idle);

        // Quick connect.
        ui.horizontal(|ui| {
            ui.heading("Connect");
            if ui.button("Settings").clicked() {
                self.settings_editor = crate::settings::Editor::default();
                self.show_settings = true;
            }
        });
        ui.add_space(4.0);
        ui.add_enabled_ui(!busy, |ui| {
            egui::Grid::new("quick")
                .num_columns(2)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Address");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.address)
                            .hint_text(address_hint(self.internet))
                            .desired_width(240.0),
                    );
                    ui.end_row();
                    ui.label("Access code");
                    let code = ui.add(
                        egui::TextEdit::singleline(&mut self.code)
                            .hint_text("XXXX-XXXX-XX")
                            .desired_width(240.0),
                    );
                    if std::mem::take(&mut self.focus_code) {
                        code.request_focus();
                    }
                    ui.end_row();
                    let ready = !self.address.trim().is_empty() && !self.code.trim().is_empty();
                    if ready && code.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let target = self.quick_target();
                        self.connect(&ctx, target);
                    }
                });
            if connect::parse_device_id(&self.address).is_some() {
                ui.small("Device ID: found through TideDesk's rendezvous service (or the one set in Settings).");
            }
            ui.checkbox(&mut self.sound, "Play sound from the remote computer");
            ui.checkbox(&mut self.internet, INTERNET_OPTION);
            ui.horizontal(|ui| {
                let ready = !self.address.trim().is_empty() && !self.code.trim().is_empty();
                if ui
                    .add_enabled(ready, egui::Button::new("Connect"))
                    .clicked()
                {
                    let target = self.quick_target();
                    self.connect(&ctx, target);
                }
                let known = self.book.find_by_address(&self.address).is_some();
                if !self.address.trim().is_empty()
                    && !known
                    && ui.button("Save to my computers").clicked()
                {
                    let address = self.address.trim().to_string();
                    self.open_editor(None, &address);
                    if let Some(ed) = &mut self.editor {
                        ed.code = self.code.clone();
                        ed.remember_code = !self.code.trim().is_empty();
                        ed.sound = self.sound;
                        ed.internet = self.internet;
                    }
                }
            });
        });

        match &self.phase {
            Phase::Checking(_) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Contacting the computer…");
                });
            }
            Phase::Opening {
                status,
                viewer_address,
                control,
                ..
            } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(status.as_str());
                });
                if let Some(me) = viewer_address {
                    let me = me.to_string();
                    ui.label("Give this address to the person at the host:");
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&me).monospace().size(20.0).strong());
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(me.clone());
                        }
                    });
                    ui.small("They type it under \"Viewer on another network\" and press Open.");
                }
                if ui.button("Cancel").clicked() {
                    control.send(child::CANCEL);
                }
            }
            Phase::InSession(host) => {
                ui.label(format!(
                    "Connected to {host}. Close the remote window to disconnect."
                ));
            }
            _ => {}
        }
        if let Some((is_error, text)) = &self.message {
            let color = if *is_error {
                ERROR
            } else {
                ui.visuals().text_color()
            };
            ui.label(RichText::new(text).color(color));
        }
        ui.add_space(8.0);
        ui.separator();

        // Saved computers.
        ui.horizontal(|ui| {
            ui.heading("My computers");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add…").clicked() {
                    self.open_editor(None, "");
                }
            });
        });
        if self.book.computers.len() > 6 {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Search")
                    .desired_width(f32::INFINITY),
            );
        }
        if self.book.computers.is_empty() {
            ui.small("Computers you save appear here for one-click connections.");
        }

        let filter = self.filter.trim().to_lowercase();
        let mut action: Option<(usize, &'static str)> = None;
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                for (i, pc) in self.book.computers.iter().enumerate() {
                    if !filter.is_empty()
                        && !pc.name.to_lowercase().contains(&filter)
                        && !pc.address.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let label = ui
                                .add(
                                    egui::Label::new(RichText::new(&pc.name).strong())
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Double-click to connect");
                            let address = if pc.internet {
                                format!("{} · internet", pc.address)
                            } else {
                                pc.address.clone()
                            };
                            ui.small(RichText::new(address).weak());
                            if label.double_clicked() && !busy {
                                action = Some((i, "connect"));
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if self.pending_delete == Some(i) {
                                        if ui.small_button("Cancel").clicked() {
                                            action = Some((i, "keep"));
                                        }
                                        if ui
                                            .small_button(RichText::new("Delete").color(ERROR))
                                            .clicked()
                                        {
                                            action = Some((i, "delete"));
                                        }
                                        return;
                                    }
                                    if ui.small_button("Delete").clicked() {
                                        action = Some((i, "ask-delete"));
                                    }
                                    if ui.small_button("Edit").clicked() {
                                        action = Some((i, "edit"));
                                    }
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("Connect").small())
                                        .clicked()
                                    {
                                        action = Some((i, "connect"));
                                    }
                                },
                            );
                        });
                    });
                }
            });

        match action {
            Some((i, "connect")) => {
                let pc = &self.book.computers[i];
                let target = Target {
                    address: pc.address.clone(),
                    code: pc.code().unwrap_or_default(),
                    sound: pc.sound,
                    internet: pc.internet,
                };
                self.connect(&ctx, target);
            }
            Some((i, "edit")) => self.open_editor(Some(i), ""),
            Some((i, "ask-delete")) => self.pending_delete = Some(i),
            Some((_, "keep")) => self.pending_delete = None,
            Some((i, "delete")) => {
                self.book.computers.remove(i);
                self.pending_delete = None;
                if let Err(e) = self.book.save() {
                    self.message = Some((true, format!("{e:#}")));
                }
            }
            _ => {}
        }

        // Hosts connected to before but never saved.
        let unsaved: Vec<String> = self
            .recent
            .iter()
            .filter(|r| self.book.find_by_address(r).is_none())
            .cloned()
            .collect();
        if !unsaved.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new("Recently connected").weak());
            for host in unsaved {
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Link::new(&host)).clicked() {
                        // An internet address can only have been reached over the internet.
                        self.internet = connect::parse_internet_host(&host).is_ok();
                        self.address = host.clone();
                        self.focus_code = true;
                    }
                    if ui.small_button("Save").clicked() {
                        self.open_editor(None, &host);
                    }
                });
            }
        }
    }

    fn quick_target(&self) -> Target {
        Target {
            address: self.address.trim().to_string(),
            code: self.code.trim().to_string(),
            sound: self.sound,
            internet: self.internet,
        }
    }

    /// The fingerprint question, for a host probed here or by a session.
    fn confirm_view(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        match &self.phase {
            Phase::Confirm(probe, target) => {
                let (probe, target) = (probe.clone(), target.clone());
                match identity_prompt(ui, &probe) {
                    Some(true) => self.trust(&ctx, &probe, &target),
                    Some(false) => self.phase = Phase::Idle,
                    None => {}
                }
            }
            Phase::ConfirmSession {
                host,
                probe,
                control,
            } => {
                let (host, probe, control) = (host.clone(), probe.clone(), control.clone());
                if let Some(trusted) = identity_prompt(ui, &probe) {
                    // The session pins the host itself, or ends and says so.
                    let (answer, status) = if trusted {
                        (child::TRUST, "Connecting…")
                    } else {
                        (child::CANCEL, "Cancelling…")
                    };
                    control.send(answer);
                    self.phase = Phase::Opening {
                        host,
                        status: status.into(),
                        viewer_address: None,
                        control,
                    };
                }
            }
            _ => {}
        }
    }
}

/// Why a computer's address cannot be saved, if it cannot.
fn address_problem(address: &str, internet: bool) -> Option<String> {
    if address.trim().is_empty() {
        return Some("Enter the computer's address.".into());
    }
    if connect::parse_device_id(address).is_some() {
        return None; // found through the rendezvous service, not by address
    }
    if internet && let Err(e) = connect::parse_internet_host(address) {
        return Some(format!("{e:#}"));
    }
    None
}

fn address_hint(internet: bool) -> &'static str {
    if internet {
        "its internet address, like 203.0.113.5:40000"
    } else {
        "192.168.1.50, my-pc or a device ID"
    }
}

/// Shows a host's fingerprint for the user to check against the host's
/// window: `Some(true)` to trust it, `Some(false)` to cancel.
fn identity_prompt(ui: &mut egui::Ui, probe: &Probe) -> Option<bool> {
    let mismatch = matches!(probe.status, PinStatus::Mismatch { .. });
    if mismatch {
        ui.label(
            RichText::new("⚠ This computer's identity has changed!")
                .color(ERROR)
                .strong(),
        );
        ui.label(
            "Someone may be intercepting the connection, or TideDesk was \
             reinstalled on it. Only continue if you are sure.",
        );
    } else {
        ui.label(format!("First connection to {}.", probe.address));
        ui.label("Check that this fingerprint matches the one in the host window:");
    }
    ui.add_space(4.0);
    ui.label(RichText::new(&probe.fingerprint).monospace());
    ui.add_space(8.0);
    let mut decision = None;
    ui.horizontal(|ui| {
        let label = if mismatch {
            "Trust new identity"
        } else {
            "It matches — connect"
        };
        if ui.button(label).clicked() {
            decision = Some(true);
        }
        if ui.button("Cancel").clicked() {
            decision = Some(false);
        }
    });
    decision
}

impl egui_software_backend::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _backend: &mut SoftwareBackend) {
        let ctx = ui.ctx().clone();
        self.handle_updates(&ctx);
        egui::Window::new("Session controls")
            .open(&mut self.show_settings)
            .resizable(true)
            .default_width(440.0)
            .show(&ctx, |ui| self.settings_editor.ui(ui));

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(4.0);
            if matches!(
                self.phase,
                Phase::Confirm(..) | Phase::ConfirmSession { .. }
            ) {
                self.confirm_view(ui);
            } else if self.editor.is_some() {
                self.editor_view(ui);
            } else {
                self.main_view(ui);
            }
        });
    }
}

/// The session process's own arguments: the host, then the route and options.
fn session_args(host: &str, sound: bool, route: &SessionRoute) -> Vec<OsString> {
    let mut args = vec![OsString::from(host)];
    if !sound {
        args.push("--no-audio".into());
    }
    match route {
        SessionRoute::Direct => {}
        SessionRoute::Internet => args.push("--internet".into()),
        SessionRoute::Rendezvous(service) => {
            args.push("--rendezvous".into());
            args.push(service.into());
        }
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_arguments_name_the_host_then_the_route() {
        assert_eq!(
            session_args("my-pc", true, &SessionRoute::Direct),
            [OsString::from("my-pc")]
        );
        assert_eq!(
            session_args("203.0.113.5:40000", false, &SessionRoute::Internet),
            ["203.0.113.5:40000", "--no-audio", "--internet"].map(OsString::from)
        );
        let service = SessionRoute::Rendezvous("rv.example:47900".into());
        assert_eq!(
            session_args("TD-1A2B-3C4D-5E6F-7A8B", true, &service),
            ["TD-1A2B-3C4D-5E6F-7A8B", "--rendezvous", "rv.example:47900"].map(OsString::from)
        );
    }

    fn launcher() -> Launcher {
        Launcher {
            address: String::new(),
            code: String::new(),
            sound: true,
            internet: false,
            focus_code: false,
            book: AddressBook::default(),
            recent: Vec::new(),
            editor: None,
            pending_delete: None,
            filter: String::new(),
            phase: Phase::Idle,
            message: None,
            inbox: Arc::default(),
            show_settings: false,
            settings_editor: crate::settings::Editor::default(),
        }
    }

    #[test]
    fn internet_session_lines_move_the_launcher_through_its_phases() {
        let ctx = egui::Context::default();
        let host = "203.0.113.5:40000".to_string();
        let mut l = launcher();
        l.on_child_line(&ctx, ChildLine::Status("stray".into()));
        assert!(
            matches!(l.phase, Phase::Idle),
            "only a connecting session is followed"
        );

        l.phase = Phase::Opening {
            host: host.clone(),
            status: "Starting…".into(),
            viewer_address: None,
            control: SessionControl(Arc::default()),
        };
        let me: SocketAddr = "198.51.100.7:51234".parse().unwrap();
        l.on_child_line(&ctx, ChildLine::Status("Waiting for the host…".into()));
        l.on_child_line(&ctx, ChildLine::ViewerAddress(me));
        assert!(matches!(
            &l.phase,
            Phase::Opening { status, viewer_address: Some(shown), .. }
                if status == "Waiting for the host…" && *shown == me
        ));

        l.on_child_line(
            &ctx,
            ChildLine::Fingerprint {
                address: host.clone(),
                fingerprint: "AAAA 1111".into(),
                status: PinStatus::Unknown,
            },
        );
        assert!(matches!(&l.phase, Phase::ConfirmSession { probe, .. } if probe.address == host));

        l.on_child_line(&ctx, ChildLine::Connected("Office".into()));
        assert!(matches!(&l.phase, Phase::InSession(h) if *h == host));

        let ended = Update::SessionEnded {
            host,
            outcome: Err(child::CANCELLED.into()),
        };
        Launcher::post(&l.inbox, &ctx, ended);
        l.handle_updates(&ctx);
        assert!(matches!(l.phase, Phase::Idle));
        assert_eq!(
            l.message,
            Some((false, "Cancelled.".into())),
            "not shown as an error"
        );
    }

    #[test]
    fn saved_internet_computers_need_an_internet_address() {
        assert_eq!(
            address_problem(" ", false).as_deref(),
            Some("Enter the computer's address.")
        );
        assert_eq!(address_problem("my-pc", false), None);
        assert!(address_problem("my-pc", true).is_some());
        assert!(address_problem("192.168.1.5:47800", true).is_some());
        assert_eq!(address_problem("203.0.113.5:40000", true), None);
    }
}
