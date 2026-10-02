//! TideDesk's look, shared by every window: deep navy, one teal accent, IBM Plex (SIL Open Font
//! License, bundled with its licence). Applied once per window.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, Theme, vec2};

pub const BG: Color32 = Color32::from_rgb(0x0D, 0x15, 0x22);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x0A, 0x11, 0x1C);
pub const SURFACE: Color32 = Color32::from_rgb(0x14, 0x20, 0x33);
pub const RAISED: Color32 = Color32::from_rgb(0x1A, 0x29, 0x40);
pub const SELECTED: Color32 = Color32::from_rgb(0x17, 0x24, 0x38);
pub const FIELD: Color32 = Color32::from_rgb(0x0F, 0x1A, 0x2A);
pub const BORDER: Color32 = Color32::from_rgb(0x23, 0x34, 0x4D);
pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xED, 0xF6);
pub const MUTED: Color32 = Color32::from_rgb(0x8F, 0xA2, 0xBA);
pub const ACCENT: Color32 = Color32::from_rgb(0x33, 0xC3, 0xB0);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x06, 0x22, 0x1F);
pub const READY: Color32 = Color32::from_rgb(0x9F, 0xE3, 0xD9);
pub const OK: Color32 = Color32::from_rgb(0x4C, 0xC9, 0x8A);
pub const WARN_BG: Color32 = Color32::from_rgb(0x2A, 0x23, 0x12);
pub const WARN: Color32 = Color32::from_rgb(0xF2, 0xD4, 0x8A);
pub const DANGER_BG: Color32 = Color32::from_rgb(0x5A, 0x2A, 0x2A);
pub const DANGER: Color32 = Color32::from_rgb(0xFF, 0xD2, 0xCF);

/// Corner radii and sizes, so every screen matches.
pub const CARD_RADIUS: u8 = 12;
pub const CONTROL_RADIUS: u8 = 8;
pub const BUTTON_HEIGHT: f32 = 32.0;

/// The family for headings and strong labels (IBM Plex Sans SemiBold).
pub fn strong() -> FontFamily {
    FontFamily::Name("strong".into())
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        "plex",
        include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf"),
    );
    add(
        "plex-semibold",
        include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    );
    add(
        "plex-mono",
        include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf"),
    );
    // egui's own fonts stay behind ours for the symbols Plex lacks.
    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "plex".into());
    let fallback = proportional.clone();
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".into());
    let mut bold = vec!["plex-semibold".to_string()];
    bold.extend(fallback);
    fonts.families.insert(strong(), bold);
    fonts
}

/// Fonts, colours, corners and spacing for a window.
pub fn apply(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.set_theme(Theme::Dark);
    ctx.style_mut_of(Theme::Dark, |style| {
        use egui::{FontId, TextStyle};
        style.text_styles = [
            (TextStyle::Heading, FontId::new(22.0, strong())),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(14.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(12.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
        ]
        .into();
        let spacing = &mut style.spacing;
        spacing.item_spacing = vec2(8.0, 8.0);
        spacing.button_padding = vec2(14.0, 7.0);
        spacing.interact_size.y = BUTTON_HEIGHT;
        // Solid bars: shown whenever a list scrolls, not only under the mouse.
        spacing.scroll = egui::style::ScrollStyle {
            bar_width: 8.0,
            // The handle in the text's colour, to stand out from its track.
            foreground_color: true,
            ..egui::style::ScrollStyle::solid()
        };

        let v = &mut style.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.window_stroke = Stroke::new(1.0_f32, BORDER);
        v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
        v.menu_corner_radius = CornerRadius::same(CONTROL_RADIUS);
        v.extreme_bg_color = FIELD;
        v.faint_bg_color = SURFACE;
        v.code_bg_color = FIELD;
        v.hyperlink_color = ACCENT;
        v.warn_fg_color = WARN;
        v.selection.bg_fill = Color32::from_rgb(0x1C, 0x4A, 0x45);
        v.selection.stroke = Stroke::new(1.0_f32, ACCENT);
        v.override_text_color = None;
        for (widget, fill) in [
            (&mut v.widgets.noninteractive, SURFACE),
            (&mut v.widgets.inactive, RAISED),
            (&mut v.widgets.hovered, Color32::from_rgb(0x22, 0x35, 0x52)),
            (&mut v.widgets.active, Color32::from_rgb(0x28, 0x3E, 0x60)),
            (&mut v.widgets.open, RAISED),
        ] {
            widget.corner_radius = CornerRadius::same(CONTROL_RADIUS);
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(0x2C, 0x40, 0x60));
        v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, ACCENT);
        v.widgets.active.fg_stroke = Stroke::new(1.5_f32, TEXT);
    });
}

/// A card: the surface, its border and corners, and room inside.
pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::same(16))
}

/// A banner in `fill`, for warnings and notices.
pub fn banner(fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(egui::Margin::symmetric(14, 10))
}

/// The one teal button a screen has.
pub fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_string())
            .color(ON_ACCENT)
            .family(strong()),
    )
    .fill(ACCENT)
    .corner_radius(CornerRadius::same(CONTROL_RADIUS))
}

/// A heading line in the strong face.
pub fn title(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_string())
        .family(strong())
        .size(24.0)
        .color(TEXT)
}

/// A label in the strong face.
pub fn label(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_string())
        .family(strong())
        .color(TEXT)
}

/// A rounded status pill with a dot.
pub fn pill(ui: &mut egui::Ui, dot: Color32, fill: Color32, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, dot);
                ui.label(egui::RichText::new(text.to_string()).color(color));
            });
        });
}

/// A button that copies `text` and says "Copied" for a moment after.
pub fn copy(ui: &mut egui::Ui, label: &str, text: &str, primary_look: bool) -> egui::Response {
    let id = ui.make_persistent_id(("copy", label, text));
    let now = ui.input(|i| i.time);
    let copied_at: Option<f64> = ui.ctx().data(|d| d.get_temp(id));
    let recent = copied_at.is_some_and(|at| now - at < 1.5);
    let shown = if recent { "Copied" } else { label };
    let response = if primary_look {
        ui.add(primary(shown))
    } else {
        ui.button(shown)
    };
    if response.clicked() {
        ui.ctx().copy_text(text.to_string());
        ui.ctx().data_mut(|d| d.insert_temp(id, now));
    }
    if recent {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
    response
}

/// The sidebar's line icons, drawn on a 24-unit grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Computer,
    Connect,
    History,
    Settings,
    About,
}

/// Draws `icon` in `rect` with `color`.
pub fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon, color: Color32) {
    use egui::{Pos2, StrokeKind};
    let unit = rect.width().min(rect.height()) / 24.0;
    let at = |x: f32, y: f32| Pos2::new(rect.left() + x * unit, rect.top() + y * unit);
    let stroke = Stroke::new(1.8_f32 * unit.max(0.6), color);
    let line = |points: &[(f32, f32)]| {
        let points: Vec<Pos2> = points.iter().map(|&(x, y)| at(x, y)).collect();
        painter.add(egui::Shape::line(points, stroke));
    };
    match icon {
        Icon::Computer => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(3.0, 4.0), at(21.0, 16.0)),
                CornerRadius::same(2),
                stroke,
                StrokeKind::Middle,
            );
            line(&[(8.0, 20.0), (16.0, 20.0)]);
            line(&[(12.0, 16.0), (12.0, 20.0)]);
        }
        Icon::Connect => {
            line(&[(5.0, 12.0), (19.0, 12.0)]);
            line(&[(13.0, 6.0), (19.0, 12.0), (13.0, 18.0)]);
        }
        Icon::History => {
            painter.circle_stroke(at(12.0, 12.0), 8.0 * unit, stroke);
            line(&[(12.0, 8.0), (12.0, 12.0), (15.0, 14.0)]);
        }
        Icon::Settings => {
            line(&[(4.0, 7.0), (14.0, 7.0)]);
            line(&[(18.0, 7.0), (20.0, 7.0)]);
            line(&[(4.0, 17.0), (8.0, 17.0)]);
            line(&[(12.0, 17.0), (20.0, 17.0)]);
            painter.circle_stroke(at(16.0, 7.0), 2.0 * unit, stroke);
            painter.circle_stroke(at(10.0, 17.0), 2.0 * unit, stroke);
        }
        Icon::About => {
            painter.circle_stroke(at(12.0, 12.0), 8.0 * unit, stroke);
            line(&[(12.0, 11.0), (12.0, 16.0)]);
            painter.circle_filled(at(12.0, 8.0), 1.2 * unit, color);
        }
    }
}

/// One sidebar entry: its icon and name, the whole row clickable.
pub fn nav_item(ui: &mut egui::Ui, icon: Icon, text: &str, selected: bool) -> egui::Response {
    let size = vec2(ui.available_width(), 38.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if selected {
        SELECTED
    } else if response.hovered() {
        Color32::from_rgb(0x11, 0x1B, 0x2B)
    } else {
        SIDEBAR
    };
    let color = if selected || response.hovered() {
        TEXT
    } else {
        MUTED
    };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(CONTROL_RADIUS), fill);
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 22.0, rect.center().y),
        vec2(18.0, 18.0),
    );
    paint_icon(painter, icon_rect, icon, color);
    painter.text(
        egui::pos2(rect.left() + 42.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::new(
            15.0,
            if selected {
                strong()
            } else {
                FontFamily::Proportional
            },
        ),
        color,
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A chat message: theirs on the left, yours on the right.
pub fn bubble(ui: &mut egui::Ui, mine: bool, text: &str) {
    let layout = if mine {
        egui::Layout::right_to_left(egui::Align::Min)
    } else {
        egui::Layout::left_to_right(egui::Align::Min)
    };
    let width = ui.available_width() * 0.75;
    ui.with_layout(layout, |ui| {
        egui::Frame::new()
            .fill(if mine {
                Color32::from_rgb(0x1C, 0x4A, 0x45)
            } else {
                Color32::from_rgb(0x1F, 0x35, 0x50)
            })
            .corner_radius(CornerRadius::same(CARD_RADIUS))
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(width);
                ui.add(egui::Label::new(egui::RichText::new(text.to_string()).color(TEXT)).wrap());
            });
    });
}

/// "12 min", "1 h 5 min": how long ago, for a session card.
pub fn since(seconds: u64) -> String {
    match seconds / 60 {
        0 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m => format!("{} h {} min ago", m / 60, m % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fonts_load_and_the_strong_family_exists() {
        let fonts = fonts();
        for name in ["plex", "plex-semibold", "plex-mono"] {
            assert!(fonts.font_data.contains_key(name), "{name}");
        }
        assert_eq!(fonts.families[&strong()][0], "plex-semibold");
        assert_eq!(fonts.families[&FontFamily::Monospace][0], "plex-mono");
        // Text on the teal button and grey text on cards stay readable.
        fn luminance(c: Color32) -> f32 {
            let f = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        let contrast = |a: Color32, b: Color32| {
            let (x, y) = (luminance(a), luminance(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        assert!(contrast(ON_ACCENT, ACCENT) >= 4.5);
        assert!(contrast(MUTED, SURFACE) >= 4.5);
        assert!(contrast(TEXT, BG) >= 7.0);
        assert!(contrast(WARN, WARN_BG) >= 4.5);
    }

    #[test]
    fn scrollbars_show_whenever_a_list_scrolls() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let scroll = ctx.style_of(Theme::Dark).spacing.scroll;
        assert!(
            !scroll.floating,
            "a floating bar is hidden until the mouse is near it"
        );
        assert!(scroll.bar_width >= 8.0, "wide enough to grab");
    }

    #[test]
    fn time_since_reads_well() {
        assert_eq!(since(20), "just now");
        assert_eq!(since(12 * 60 + 5), "12 min ago");
        assert_eq!(since(65 * 60), "1 h 5 min ago");
    }
}
