use eframe::egui;

/// Popup panel background alpha (0–255). Lower = more see-through.
/// On Windows, acrylic/mica from `window_effects` carries most of the glass look.
pub const POPUP_BG_ALPHA: u8 = 200;

/// Semi-transparent fill for the converter popup (requires viewport transparency).
#[must_use]
pub fn popup_panel_fill() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(24, 24, 28, POPUP_BG_ALPHA)
}

/// Applies a modern dark theme to the egui context.
pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    // Modern dark color palette (alpha for popup over desktop)
    visuals.panel_fill = egui::Color32::from_rgba_unmultiplied(18, 18, 18, POPUP_BG_ALPHA);
    visuals.window_fill = egui::Color32::from_rgba_unmultiplied(24, 24, 28, POPUP_BG_ALPHA);
    visuals.extreme_bg_color = egui::Color32::from_rgba_unmultiplied(12, 12, 12, POPUP_BG_ALPHA);

    // Borders and separators
    visuals.widgets.inactive.bg_fill =
        egui::Color32::from_rgba_unmultiplied(35, 35, 40, POPUP_BG_ALPHA);
    visuals.widgets.hovered.bg_fill =
        egui::Color32::from_rgba_unmultiplied(45, 45, 52, POPUP_BG_ALPHA);
    visuals.widgets.noninteractive.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(45, 45, 45));
    visuals.widgets.noninteractive.fg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(180, 180, 180));

    // Rounded corners
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(6);

    // Remove shadows for a flat, modern look
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.popup_shadow = egui::Shadow::NONE;

    // Accent color (modern blue)
    let accent_color = egui::Color32::from_rgb(80, 140, 250);
    visuals.selection.bg_fill = accent_color;

    ctx.set_visuals(visuals);

    // Global spacing and fonts
    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.window_margin = egui::Margin::same(15);
    style.spacing.button_padding = egui::vec2(8.0, 4.0);

    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));

    ctx.set_global_style(style);
}
