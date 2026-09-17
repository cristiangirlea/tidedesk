//! The viewer's main window: quick connect plus a list of saved computers.
//!
//! The remote-screen window runs in a child process: winit allows one event
//! loop per process, and a separate process also keeps this window responsive
//! and able to report why a session ended.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use egui::{Color32, RichText};
use egui_software_backend::{SoftwareBackend, SoftwareBackendAppConfiguration};
use tidedesk_core::identity::{KnownHosts, PinStatus};
use tidedesk_core::paths;

use crate::computers::{AddressBook, Computer};
use crate::connect::{self, Probe};
use crate::icon;

const ERROR: Color32 = Color32::from_rgb(220, 60, 50);

/// A connection about to be made.
#[derive(Clone)]
struct Target {
    address: String,
    code: String,
    sound: bool,
}

enum Phase {
    Idle,
    Checking(Target),
    /// The host's identity needs the user's approval.
    Confirm(Probe, Target),
    InSession(String),
}

/// Results handed back from worker threads.
enum Update {
    Probed(Result<Probe>),
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
    error: Option<String>,
}

struct Launcher {
    // Quick connect.
    address: String,
    code: String,
    sound: bool,
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

    fn start_session(&mut self, ctx: &egui::Context, host: String, target: &Target) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return self.fail(format!("cannot locate viewer: {e}")),
        };
        let mut cmd = Command::new(exe);
        cmd.arg(&host)
            // The code travels in the environment, not the (visible) command line.
            .env("TIDEDESK_CODE", target.code.trim())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if !target.sound {
            cmd.arg("--no-audio");
        }
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

        self.phase = Phase::InSession(host.clone());
        self.message = None;
        let (inbox, ctx2) = (self.inbox.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut stderr = String::new();
            if let Some(mut s) = child.stderr.take() {
                let _ = s.read_to_string(&mut stderr);
            }
            let status = child.wait();
            // The session prints `error: …` or `disconnected: …` as its last word.
            let last = |prefix: &str| {
                stderr
                    .lines()
                    .rev()
                    .find_map(|l| l.strip_prefix(prefix).map(str::to_string))
            };
            let outcome = match status {
                Ok(s) if s.success() => Ok(()),
                _ => {
                    Err(last("error: ").unwrap_or_else(|| "the session ended unexpectedly".into()))
                }
            };
            let outcome = match (outcome, last("disconnected: ")) {
                (Ok(()), Some(reason)) if reason != "host ended the session" => Err(reason),
                (o, _) => o,
            };
            ctx2.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            Self::post(&inbox, &ctx2, Update::SessionEnded { host, outcome });
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
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
                            self.start_session(ctx, probe.address, &target)
                        }
                        Ok(probe) => self.phase = Phase::Confirm(probe, target),
                    }
                }
                Update::SessionEnded { host, outcome } => {
                    self.phase = Phase::Idle;
                    self.message = Some(match outcome {
                        Ok(()) => (false, format!("Session with {host} ended.")),
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
            Ok(()) => self.start_session(ctx, probe.address.clone(), target),
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
            error: None,
        });
    }

    fn confirm_view(&mut self, ui: &mut egui::Ui, probe: Probe, target: Target) {
        let ctx = ui.ctx().clone();
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
        ui.horizontal(|ui| {
            let label = if mismatch {
                "Trust new identity"
            } else {
                "It matches — connect"
            };
            if ui.button(label).clicked() {
                self.trust(&ctx, &probe, &target);
            }
            if ui.button("Cancel").clicked() {
                self.phase = Phase::Idle;
            }
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
                        .hint_text("192.168.1.50 or my-pc")
                        .desired_width(240.0),
                );
                ui.end_row();
            });
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
                if address.is_empty() {
                    ed.error = Some("Enter the computer's address.".into());
                    return;
                }
                let mut pc = ed
                    .index
                    .and_then(|i| self.book.computers.get(i).cloned())
                    .unwrap_or_else(|| Computer::new(String::new(), String::new(), true));
                pc.name = if name.is_empty() { address } else { name }.to_string();
                pc.address = address.to_string();
                pc.sound = ed.sound;
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
                            .hint_text("192.168.1.50 or my-pc")
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
            ui.checkbox(&mut self.sound, "Play sound from the remote computer");
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
                            ui.small(RichText::new(&pc.address).weak());
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
        }
    }
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
            if let Phase::Confirm(probe, target) = &self.phase {
                let (probe, target) = (probe.clone(), target.clone());
                self.confirm_view(ui, probe, target);
            } else if self.editor.is_some() {
                self.editor_view(ui);
            } else {
                self.main_view(ui);
            }
        });
    }
}
