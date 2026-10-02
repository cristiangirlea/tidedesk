//! The viewer's chat window, a process of its own as the settings window
//! is. The session writes what it should show on the window's standard
//! input and reads what is written here from its standard output, one
//! [`Line`] per line.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tidedesk_core::chat::{self, Line};

const SIZE: [f32; 2] = [380.0, 460.0];

struct Window {
    lines: Arc<Mutex<Vec<Line>>>,
    draft: String,
}

impl Window {
    fn send(&mut self) {
        let Some(text) = chat::clean(&self.draft) else {
            return;
        };
        let line = Line::Mine(text);
        let mut out = std::io::stdout().lock();
        if writeln!(out, "{}", line.encode())
            .and_then(|_| out.flush())
            .is_ok()
        {
            self.lines.lock().unwrap().push(line);
            self.draft.clear();
        }
    }
}

impl egui_software_backend::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut egui_software_backend::SoftwareBackend) {
        let ended = self
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| matches!(l, Line::Ended(_)));
        egui::Panel::bottom("write").show_inside(ui, |ui| {
            ui.add_space(6.0);
            if ended {
                ui.label("The session has ended.");
            } else {
                // Enter sends; Shift+Enter starts a new line.
                let enter =
                    ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                let edit = ui.add(
                    egui::TextEdit::multiline(&mut self.draft)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .char_limit(chat::MAX_CHARS)
                        .hint_text("Write to the host; Enter sends"),
                );
                if (enter && edit.has_focus()) | ui.button("Send").clicked() {
                    self.send();
                    edit.request_focus();
                }
            }
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in self.lines.lock().unwrap().iter() {
                        match line {
                            Line::Mine(text) => {
                                ui.label(egui::RichText::new("You").small().strong());
                                ui.label(text);
                            }
                            Line::Theirs(text) => {
                                ui.label(
                                    egui::RichText::new("Host")
                                        .small()
                                        .strong()
                                        .color(ui.visuals().hyperlink_color),
                                );
                                ui.label(text);
                            }
                            Line::Ended(why) => {
                                ui.label(
                                    egui::RichText::new(format!("Session ended: {why}")).italics(),
                                );
                            }
                        }
                        ui.add_space(4.0);
                    }
                });
        });
    }
}

/// Opens the chat window; what the session writes on standard input shows
/// in it.
pub fn run(title: &str) -> Result<()> {
    let shown: Arc<Mutex<Vec<Line>>> = Arc::default();
    let mut config = egui_software_backend::SoftwareBackendAppConfiguration::new();
    config.viewport_builder = egui::ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(SIZE)
        .with_min_inner_size([280.0, 240.0])
        .with_icon(crate::icon::egui_icon());
    egui_software_backend::run_app_with_software_backend(config, move |ctx| {
        let (lines, ctx) = (shown.clone(), ctx.clone());
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                if let Some(line) = Line::decode(&line) {
                    lines.lock().unwrap().push(line);
                    ctx.request_repaint();
                }
            }
            // The session is gone: so is its chat.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        });
        Window {
            lines: shown.clone(),
            draft: String::new(),
        }
    })
    .map_err(|e| anyhow::anyhow!("cannot open the chat: {e}"))
}
