//! Vzhled: tmavý a světlý motiv (případně podle nastavení Windows), písma, rozměry.
//! Barvy jsou funkce, protože se mění za běhu s přepnutím motivu.

use eframe::egui::{self, Color32, FontId, TextStyle};
use std::sync::atomic::{AtomicBool, Ordering};

/// Výška všech ovládacích prvků v lištách (tlačítka, vstupní pole, rozbalovací seznamy).
pub const CONTROL_H: f32 = 30.0;

static LIGHT: AtomicBool = AtomicBool::new(false);

/// Je aktivní světlý motiv?
pub fn is_light() -> bool {
    LIGHT.load(Ordering::Relaxed)
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn pick(dark: Color32, light: Color32) -> Color32 {
    if is_light() { light } else { dark }
}

pub fn accent() -> Color32 {
    pick(rgb(0xDB, 0xB4, 0x7C), rgb(0xC8, 0x95, 0x4E))
}
/// Text na akcentové výplni.
pub fn accent_text() -> Color32 {
    rgb(0x1E, 0x1A, 0x14)
}
pub fn bg() -> Color32 {
    pick(rgb(0x1F, 0x1F, 0x1F), rgb(0xF6, 0xF5, 0xF2))
}
pub fn sidebar() -> Color32 {
    pick(rgb(0x1A, 0x1A, 0x1A), rgb(0xEC, 0xEA, 0xE5))
}
pub fn dialog() -> Color32 {
    pick(rgb(0x2C, 0x2C, 0x2C), rgb(0xFF, 0xFF, 0xFF))
}
pub fn card() -> Color32 {
    pick(rgb(0x2B, 0x2B, 0x2B), rgb(0xFF, 0xFF, 0xFF))
}
pub fn card_stroke() -> Color32 {
    pick(rgb(0x38, 0x38, 0x38), rgb(0xDD, 0xDA, 0xD3))
}
pub fn muted() -> Color32 {
    pick(rgb(0x9A, 0x9A, 0x9A), rgb(0x6A, 0x68, 0x63))
}
pub fn green() -> Color32 {
    pick(rgb(0x3F, 0xC2, 0x5A), rgb(0x1E, 0x8E, 0x3E))
}
pub fn red() -> Color32 {
    pick(rgb(0xE5, 0x5B, 0x4F), rgb(0xC8, 0x3A, 0x2E))
}
pub fn yellow() -> Color32 {
    pick(rgb(0xE3, 0xB3, 0x41), rgb(0xA8, 0x74, 0x00))
}
pub fn grey() -> Color32 {
    pick(rgb(0x80, 0x80, 0x80), rgb(0x8A, 0x87, 0x80))
}
/// Výplň řádku tabulky pod myší / sudého řádku.
pub fn row_hover() -> Color32 {
    pick(Color32::from_gray(0x3A), rgb(0xEA, 0xE7, 0xE0))
}
pub fn row_stripe() -> Color32 {
    pick(Color32::from_gray(0x2F), rgb(0xF4, 0xF2, 0xEE))
}
/// Výrazný text (vybraná položka navigace, text potvrzovacího dialogu).
pub fn strong_text() -> Color32 {
    pick(Color32::WHITE, rgb(0x1C, 0x1B, 0x19))
}
pub fn body_text() -> Color32 {
    pick(Color32::from_gray(210), rgb(0x2E, 0x2C, 0x29))
}
/// Rozlišovací barva podle odstínu (0..1) – čitelná na pozadí obou motivů
/// (např. služby ve sloučeném logu stacku).
pub fn hue(h: f32) -> Color32 {
    let (s, v) = if is_light() { (0.85, 0.50) } else { (0.50, 0.95) };
    egui::ecolor::Hsva::new(h.rem_euclid(1.0), s, v, 1.0).into()
}

/// Režim z nastavení: "dark", "light" nebo "system" (podle Windows).
pub fn resolve(mode: &str) -> bool {
    match mode {
        "light" => true,
        "dark" => false,
        _ => windows_uses_light(),
    }
}

/// Windows: HKCU\…\Themes\Personalize\AppsUseLightTheme (1 = světlý režim aplikací).
#[cfg(windows)]
pub fn windows_uses_light() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (k, n) = (w(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"), w("AppsUseLightTheme"));
    let mut v: u32 = 0;
    let mut len = 4u32;
    // SAFETY: platné nulou ukončené řetězce, výstup do lokální u32 s velikostí 4 B
    let r = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut v as *mut u32).cast(), &mut len)
    };
    r == 0 && v == 1
}

#[cfg(not(windows))]
pub fn windows_uses_light() -> bool {
    false
}

/// Nastaví motiv ("dark" / "light" / "system") a styl egui. Lze volat kdykoli za běhu.
pub fn set_mode(ctx: &egui::Context, mode: &str) {
    LIGHT.store(resolve(mode), Ordering::Relaxed);
    style(ctx);
}

pub fn apply(ctx: &egui::Context, mode: &str) {
    // ikonový font Phosphor jako záložní font – ikony se pak dají psát přímo do textu
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
    set_mode(ctx, mode);
}

fn style(ctx: &egui::Context) {
    let light = is_light();
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.style_mut(|s| {
        s.visuals = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
        let v = &mut s.visuals;
        v.panel_fill = bg();
        if light {
            v.window_fill = rgb(0xFB, 0xFA, 0xF8);
            v.extreme_bg_color = rgb(0xFF, 0xFF, 0xFF);
            v.faint_bg_color = rgb(0xF0, 0xEE, 0xE9);
            v.widgets.inactive.weak_bg_fill = rgb(0xE7, 0xE4, 0xDE);
            v.widgets.hovered.weak_bg_fill = rgb(0xDD, 0xD9, 0xD1);
            v.widgets.active.weak_bg_fill = rgb(0xD2, 0xCD, 0xC3);
            v.widgets.inactive.bg_fill = rgb(0xE7, 0xE4, 0xDE);
            v.widgets.noninteractive.bg_stroke.color = card_stroke();
            v.window_stroke.color = card_stroke();
            v.override_text_color = None;
        } else {
            v.window_fill = rgb(0x27, 0x27, 0x27);
            v.extreme_bg_color = rgb(0x16, 0x16, 0x16);
            v.faint_bg_color = rgb(0x26, 0x26, 0x26);
            v.widgets.inactive.weak_bg_fill = rgb(0x33, 0x33, 0x33);
            v.widgets.hovered.weak_bg_fill = rgb(0x3D, 0x3D, 0x3D);
            v.widgets.active.weak_bg_fill = rgb(0x48, 0x48, 0x48);
        }
        v.selection.bg_fill = accent().linear_multiply(if light { 0.45 } else { 0.35 });
        v.selection.stroke.color = if light { rgb(0x6B, 0x4A, 0x16) } else { accent() };
        v.hyperlink_color = if light { rgb(0x8A, 0x5E, 0x1E) } else { accent() };
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.widgets.inactive.corner_radius = egui::CornerRadius::same(5);
        v.widgets.hovered.corner_radius = egui::CornerRadius::same(5);
        v.widgets.active.corner_radius = egui::CornerRadius::same(5);
        // ruka nad vším klikacím (tlačítka, ikony, odkazy)
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);
        s.spacing.window_margin = egui::Margin::same(14);
        // posuvník: tenký, plovoucí nad obsahem, bez podkladu, tlumený; po najetí se rozšíří
        s.spacing.scroll = egui::style::ScrollStyle {
            floating: true,
            bar_width: 8.0,
            floating_width: 4.0,
            floating_allocated_width: 0.0,
            handle_min_length: 24.0,
            bar_inner_margin: 2.0,
            bar_outer_margin: 0.0,
            foreground_color: true,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            dormant_handle_opacity: 0.25,
            active_handle_opacity: 0.4,
            interact_handle_opacity: 0.65,
        };
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
