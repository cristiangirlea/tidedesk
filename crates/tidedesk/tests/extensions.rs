//! What another program building on TideDesk relies on: it compiles against
//! the library from outside, as such a program does.

use tidedesk_app::{Extensions, Page, egui};

struct Notes;

impl Page for Notes {
    fn label(&self) -> &str {
        "Notes"
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label("A page of its own.");
    }
}

#[test]
fn another_program_adds_pages_and_start_up_work() {
    let plain = Extensions::default();
    assert!(plain.pages.is_empty() && plain.settings.is_empty());
    assert!(plain.about.is_none() && plain.on_start.is_none());

    let added = Extensions {
        pages: vec![Box::new(Notes)],
        settings: vec![Box::new(Notes)],
        about: Some(Box::new(Notes)),
        on_start: Some(Box::new(|| {})),
    };
    assert_eq!(added.pages[0].label(), "Notes");
    assert_eq!(added.pages[0].about(), "", "about is optional");
}
