use eframe::egui::{self, Color32, FontId, TextStyle};

/// Výška všech ovládacích prvků v lištách (tlačítka, vstupní pole, rozbalovací seznamy).
pub const CONTROL_H: f32 = 30.0;

pub const ACCENT: Color32 = Color32::from_rgb(0xDB, 0xB4, 0x7C);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x1E, 0x1A, 0x14);
pub const BG: Color32 = Color32::from_rgb(0x1F, 0x1F, 0x1F);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x1A, 0x1A, 0x1A);
pub const DIALOG: Color32 = Color32::from_rgb(0x2C, 0x2C, 0x2C);
pub const CARD: Color32 = Color32::from_rgb(0x2B, 0x2B, 0x2B);
pub const CARD_STROKE: Color32 = Color32::from_rgb(0x38, 0x38, 0x38);
pub const MUTED: Color32 = Color32::from_rgb(0x9A, 0x9A, 0x9A);
pub const GREEN: Color32 = Color32::from_rgb(0x3F, 0xC2, 0x5A);
pub const RED: Color32 = Color32::from_rgb(0xE5, 0x5B, 0x4F);
pub const YELLOW: Color32 = Color32::from_rgb(0xE3, 0xB3, 0x41);
pub const GREY: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);

pub fn apply(ctx: &egui::Context) {
    // ikonový font Phosphor jako záložní font – ikony se pak dají psát přímo do textu
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|s| {
        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = Color32::from_rgb(0x27, 0x27, 0x27);
        v.extreme_bg_color = Color32::from_rgb(0x16, 0x16, 0x16);
        v.faint_bg_color = Color32::from_rgb(0x26, 0x26, 0x26);
        v.selection.bg_fill = ACCENT.linear_multiply(0.35);
        v.selection.stroke.color = ACCENT;
        v.hyperlink_color = ACCENT;
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x33, 0x33, 0x33);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3D, 0x3D, 0x3D);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(0x48, 0x48, 0x48);
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(5);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(5);
        v.widgets.active.corner_radius = egui::CornerRadius::same(5);
        // ruka nad vším klikacím (tlačítka, ikony, odkazy)
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);
        s.spacing.window_margin = egui::Margin::same(14);
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        // jednotná výška ovládacích prvků (tlačítka, pole, seznamy, menu) = CONTROL_H
        s.spacing.button_padding = egui::vec2(10.0, 6.0);
        s.spacing.interact_size.y = CONTROL_H;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(16.0)); // titulky oken
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
}

/// Ikona okna (stejná kresba jako ikona EXE).
pub fn app_icon() -> egui::IconData {
    let n = 64u32;
    egui::IconData { rgba: crate::icon_art::render(n, None), width: n, height: n }
}
