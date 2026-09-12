//! KDE Breeze and GNOME Adwaita colour palettes + egui theme application.

use egui::{Color32, Context, CornerRadius, FontId, Stroke, Style, Visuals};

// ── Theme selection ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum AppTheme {
    BreezeDark,
    BreezeLight,
    AdwaitaDark,
    AdwaitaLight,
}

impl AppTheme {
    pub fn label(&self) -> &'static str {
        match self {
            AppTheme::BreezeDark => "Breeze Dark",
            AppTheme::BreezeLight => "Breeze Light",
            AppTheme::AdwaitaDark => "Adwaita Dark",
            AppTheme::AdwaitaLight => "Adwaita Light",
        }
    }

    /// Stable string key stored in config.toml.
    pub fn to_str(&self) -> &'static str {
        match self {
            AppTheme::BreezeDark => "BreezeDark",
            AppTheme::BreezeLight => "BreezeLight",
            AppTheme::AdwaitaDark => "AdwaitaDark",
            AppTheme::AdwaitaLight => "AdwaitaLight",
        }
    }

    /// Parse from config.toml value; unknown or empty → the desktop's native
    /// theme family (see [`default_theme`]), so a fresh install doesn't put a
    /// Breeze-styled window in the middle of a GNOME session.
    pub fn from_str(s: &str) -> Self {
        match s {
            "BreezeDark" => AppTheme::BreezeDark,
            "BreezeLight" => AppTheme::BreezeLight,
            "AdwaitaDark" => AppTheme::AdwaitaDark,
            "AdwaitaLight" => AppTheme::AdwaitaLight,
            _ => default_theme(),
        }
    }
}

/// Theme for a config that has never chosen one: Adwaita on GNOME-family
/// desktops, Breeze everywhere else. Dark in both cases — matching the
/// desktop's light/dark preference would need a DBus portal round-trip, and
/// the picker in Settings is one click away.
pub fn default_theme() -> AppTheme {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let gnome_like = desktop.split(':').any(|d| {
        matches!(
            d.to_ascii_lowercase().as_str(),
            "gnome" | "ubuntu" | "unity"
        )
    });
    if gnome_like {
        AppTheme::AdwaitaDark
    } else {
        AppTheme::BreezeDark
    }
}

/// Returns the WINDOW_BG (r, g, b) for the active theme (used by opacity fallback).
pub fn window_bg_rgb(theme: &AppTheme) -> (u8, u8, u8) {
    match theme {
        AppTheme::BreezeDark => (0x31, 0x36, 0x3b),
        AppTheme::BreezeLight => (0xef, 0xf0, 0xf1),
        AppTheme::AdwaitaDark => (0x24, 0x24, 0x24),
        AppTheme::AdwaitaLight => (0xfa, 0xfa, 0xfa),
    }
}

/// Apply opacity to the panel/window fills of a child viewport context.
/// Call at the START of a `show_viewport_immediate` callback.
/// Returns the original fills so they can be restored at the END of the callback.
pub fn push_viewport_opacity(ctx: &Context, opacity: f32) -> (Color32, Color32) {
    let orig_panel = ctx.global_style().visuals.panel_fill;
    let orig_window = ctx.global_style().visuals.window_fill;
    if opacity < 0.999 {
        let a = (opacity * 255.0) as u8;
        let tint = |c: Color32| Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a);
        ctx.global_style_mut(|s| {
            s.visuals.panel_fill = tint(s.visuals.panel_fill);
            s.visuals.window_fill = tint(s.visuals.window_fill);
        });
    }
    (orig_panel, orig_window)
}

/// Restore panel/window fills saved by [`push_viewport_opacity`].
pub fn pop_viewport_opacity(ctx: &Context, saved: (Color32, Color32)) {
    ctx.global_style_mut(|s| {
        s.visuals.panel_fill = saved.0;
        s.visuals.window_fill = saved.1;
    });
}

/// Apply the selected theme.
pub fn apply_theme(ctx: &Context, native_ppp: f32, theme: &AppTheme) {
    install_fonts(ctx);
    match theme {
        AppTheme::BreezeDark => apply(ctx, native_ppp),
        AppTheme::BreezeLight => apply_light(ctx, native_ppp),
        AppTheme::AdwaitaDark => apply_adwaita(ctx, native_ppp, true),
        AppTheme::AdwaitaLight => apply_adwaita(ctx, native_ppp, false),
    }
}

// ── Fonts ─────────────────────────────────────────────────────────────────────

/// Font family name for bold text. egui's built-in fonts ship a single weight,
/// so `RichText::strong()` only brightens the colour — the design's headings,
/// KPI values and active column headers all need a real 700 weight.
pub const BOLD: &str = "bold";

/// Register the bundled DejaVu faces.
///
/// Two problems this solves at once:
///  * no bold face — every heading in the design is 700 weight, and without a
///    bold font the whole typographic hierarchy collapses to one weight;
///  * missing glyphs — the design uses ▲ ▼ ▾ ✕ ↑ ↓, none of which exist in
///    egui's bundled fonts, so they rendered as tofu boxes.
///
/// Idempotent: the fonts are only installed once per context.
fn install_fonts(ctx: &Context) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::Relaxed) {
        return;
    }

    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "dejavu",
            &include_bytes!("../../assets/fonts/DejaVuSans.ttf")[..],
        ),
        (
            "dejavu_bold",
            &include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf")[..],
        ),
        (
            "dejavu_mono",
            &include_bytes!("../../assets/fonts/DejaVuSansMono.ttf")[..],
        ),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_static(bytes)),
        );
    }

    // Proportional and monospace lead with DejaVu; the built-in faces stay on
    // as fallbacks so emoji still resolve.
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "dejavu".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "dejavu_mono".to_owned());

    // Bold is its own family — egui has no weight axis, so `FontId` with this
    // family is how a widget asks for 700.
    fonts.families.insert(
        egui::FontFamily::Name(BOLD.into()),
        vec![
            "dejavu_bold".to_owned(),
            "dejavu".to_owned(),
            "dejavu_mono".to_owned(),
        ],
    );

    ctx.set_fonts(fonts);
}

/// A bold `FontId` at the given size — use instead of `RichText::strong()`
/// wherever the design calls for a real 700 weight.
pub fn bold_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(BOLD.into()))
}

/// The highest-contrast text colour for the active theme.
///
/// Not `Visuals::strong_text_color()`: egui derives that from
/// `widgets.active.fg_stroke`, which the light theme sets to white so pressed
/// widgets read against the blue active fill. Anything calling it for emphasis
/// text therefore renders white-on-light and disappears.
pub fn strong_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::WHITE
    } else {
        Color32::from_rgb(0x1b, 0x1e, 0x21)
    }
}

/// Surface colour for cards.
///
/// Mockup 3a puts white cards on the light grey page, which is what separates
/// them; in the dark mockups the card and the page share a fill and the border
/// alone does the work, so dark keeps its transparent surface.
pub fn card_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::TRANSPARENT
    } else {
        Color32::WHITE
    }
}

/// Surface colour for a plot area inside a card.
///
/// In dark the plot sits darker than its card; in light the card is already
/// white, so `extreme_bg_color` (also white) would erase the plot's edge —
/// mockup 3a uses a faint grey there instead.
pub fn plot_fill(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        ui.visuals().extreme_bg_color
    } else {
        ui.visuals().faint_bg_color
    }
}

/// Bold rich text at the given size, in the theme's strongest text colour.
pub fn bold(ui: &egui::Ui, text: impl Into<String>, size: f32) -> egui::RichText {
    egui::RichText::new(text.into())
        .font(bold_font(size))
        .color(strong_color(ui))
}

// ── Breeze Dark palette ───────────────────────────────────────────────────────

pub struct Breeze;

impl Breeze {
    // Backgrounds
    pub const WINDOW_BG: Color32 = Color32::from_rgb(0x31, 0x36, 0x3b); // #31363b
    pub const BASE: Color32 = Color32::from_rgb(0x23, 0x26, 0x29); // #232629
    pub const ALT_BASE: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x32); // #2a2e32

    // Text (dark theme values — use ui.visuals().text_color() in widgets for theme-awareness)
    pub const TEXT: Color32 = Color32::from_rgb(0xef, 0xf0, 0xf1); // #eff0f1

    // Accent — same Breeze blue in both themes, safe to use directly for accent labels
    pub const HIGHLIGHT: Color32 = Color32::from_rgb(0x3d, 0xae, 0xe9); // #3daee9 Breeze blue
    pub const LINK: Color32 = Color32::from_rgb(0x29, 0x80, 0xb9); // #2980b9

    // Dark-theme border (only used inside theme.rs apply(); use ui.visuals() everywhere else)
    pub const BORDER: Color32 = Color32::from_rgb(0x4d, 0x4d, 0x4d); // #4d4d4d

    // Buttons / widgets
    pub const BUTTON_BG: Color32 = Color32::from_rgb(0x31, 0x36, 0x3b); // #31363b
    pub const BUTTON_HOVER: Color32 = Color32::from_rgb(0x3a, 0x3f, 0x44); // #3a3f44

    // Semantic colours — used for CPU load, status indicators, log lines
    pub const POSITIVE: Color32 = Color32::from_rgb(0x27, 0xae, 0x60); // #27ae60  green
    pub const WARNING: Color32 = Color32::from_rgb(0xf6, 0x74, 0x00); // #f67400  orange
                                                                      // NEGATIVE lives in `sem()`/`sem_for()` — widgets take the theme-aware
                                                                      // value from there rather than the dark-theme constant.
}

// ── Design tokens ─────────────────────────────────────────────────────────────
// One source of truth for type scale and spacing, so tabs stop inventing
// their own magic numbers. Use these for all new UI.

pub mod tokens {
    /// Fine print: table meta, thread lists, sublabels
    pub const FONT_SMALL: f32 = 11.0;
    /// Column headers, chip labels, badges
    pub const FONT_LABEL: f32 = 11.5;
    /// Help text under group titles
    pub const FONT_HELP: f32 = 12.5;
    /// Default body/table text
    pub const FONT_BODY: f32 = 13.5;
    /// Section headings inside cards
    pub const FONT_HEADING: f32 = 15.0;
    /// Hero status headline (Gaming Mode / ProBalance status cards)
    pub const FONT_HERO: f32 = 17.0;
    /// KPI card value
    pub const FONT_KPI: f32 = 26.0;

    /// Tight spacing inside a group
    pub const SPACE_XS: f32 = 4.0;
    /// Between related elements
    pub const SPACE_S: f32 = 8.0;
    /// Between sections/cards
    pub const SPACE_M: f32 = 12.0;

    /// Standard data row height
    pub const ROW_H: f32 = 26.0;
    /// Dense data row height (HW Monitor sensors)
    pub const ROW_H_DENSE: f32 = 22.0;
    /// Left label column width in settings forms
    pub const FORM_LABEL_W: f32 = 220.0;
    /// Label column inside dialogs — narrower than the settings pane's, which
    /// would take a third of a 560px dialog.
    pub const DIALOG_LABEL_W: f32 = 130.0;
}

// ── Component primitives ──────────────────────────────────────────────────────
//
// Shared widgets so every tab renders chips/badges/toggles identically.

/// Pill-shaped filter chip in the accent colour.
pub fn chip(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    let accent = sem(ui).accent;
    chip_colored(ui, label, active, accent)
}

/// Pill-shaped filter chip in an explicit colour (log/HW categories carry
/// their own semantic colour). Returns true when clicked; the active state
/// gets a filled tint and a trailing × hinting that clicking clears it.
pub fn chip_colored(ui: &mut egui::Ui, label: &str, active: bool, color: Color32) -> bool {
    let s = Sem {
        accent: color,
        ..sem(ui)
    };
    let text = if active {
        format!("{label} ✕")
    } else {
        label.to_string()
    };
    let galley = ui.painter().layout_no_wrap(
        text.clone(),
        egui::FontId::proportional(tokens::FONT_LABEL),
        if active {
            s.accent
        } else {
            ui.visuals().weak_text_color()
        },
    );
    let pad = egui::vec2(9.0, 4.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let fill = if active {
            tint(s.accent, 46)
        } else if hovered {
            ui.visuals().widgets.hovered.bg_fill
        } else {
            Color32::TRANSPARENT
        };
        let stroke = if active {
            Stroke::new(1.0_f32, s.accent)
        } else {
            Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color)
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(12),
            fill,
            stroke,
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(rect.min + pad, galley, Color32::WHITE);
    }
    resp.clicked()
}

/// Paint a badge into an explicit rect (for painter-driven tables).
pub fn badge_at(painter: &egui::Painter, pos: egui::Pos2, label: &str, color: Color32) {
    let galley = painter.layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(tokens::FONT_LABEL),
        color,
    );
    let pad = egui::vec2(7.0, 1.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(pos.x, pos.y - galley.size().y * 0.5 - pad.y),
        galley.size() + pad * 2.0,
    );
    painter.rect_filled(rect, CornerRadius::same(3), tint(color, 46));
    painter.galley(rect.min + pad, galley, Color32::WHITE);
}

/// Neutral outlined badge (match types, kinds).
pub fn badge_outline(ui: &mut egui::Ui, label: &str) {
    let col = ui.visuals().weak_text_color();
    badge_outline_colored(ui, label, col)
}

/// Outlined badge in an explicit colour — the mockups use this for live
/// counters ("1 throttled now"), where a filled badge would compete with the
/// status text beside it.
pub fn badge_outline_colored(ui: &mut egui::Ui, label: &str, col: Color32) {
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(tokens::FONT_LABEL),
        col,
    );
    let pad = egui::vec2(7.0, 1.0);
    let size = galley.size() + pad * 2.0;
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0_f32, col),
            egui::StrokeKind::Inside,
        );
        ui.painter().galley(rect.min + pad, galley, Color32::WHITE);
    }
}

/// iOS-style toggle switch (26×14 pill). Returns true when toggled.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool) -> bool {
    let s = sem(ui);
    let size = egui::vec2(26.0, 14.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    if ui.is_rect_visible(rect) {
        let fill = if *on {
            s.accent
        } else {
            ui.visuals().widgets.inactive.bg_fill
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(7),
            fill,
            Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );
        let cx = if *on {
            rect.right() - 7.0
        } else {
            rect.left() + 7.0
        };
        ui.painter().circle_filled(
            egui::pos2(cx, rect.center().y),
            5.0,
            if *on {
                s.on_accent
            } else {
                ui.visuals().text_color()
            },
        );
    }
    resp.clicked()
}

/// Segmented button group. Returns the index clicked, if any.
pub fn segmented(ui: &mut egui::Ui, options: &[&str], selected: usize) -> Option<usize> {
    let s = sem(ui);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (i, opt) in options.iter().enumerate() {
            let is_sel = i == selected;
            let galley = ui.painter().layout_no_wrap(
                opt.to_string(),
                egui::FontId::proportional(tokens::FONT_BODY),
                if is_sel {
                    s.on_accent
                } else {
                    ui.visuals().text_color()
                },
            );
            let pad = egui::vec2(10.0, 4.0);
            let size = galley.size() + pad * 2.0;
            let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            if ui.is_rect_visible(rect) {
                // Round only the outer corners of the group
                let r = 3;
                let radius = CornerRadius {
                    nw: if i == 0 { r } else { 0 },
                    sw: if i == 0 { r } else { 0 },
                    ne: if i == options.len() - 1 { r } else { 0 },
                    se: if i == options.len() - 1 { r } else { 0 },
                };
                let fill = if is_sel {
                    s.accent
                } else if resp.hovered() {
                    ui.visuals().widgets.hovered.bg_fill
                } else {
                    ui.visuals().widgets.inactive.bg_fill
                };
                ui.painter().rect(
                    rect,
                    radius,
                    fill,
                    Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
                    egui::StrokeKind::Inside,
                );
                ui.painter().galley(rect.min + pad, galley, Color32::WHITE);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Horizontal chrome `kpi_card` draws outside the content: its 12px inner
/// margins on both sides.
///
/// Callers divide the available width into whole card slots and the card
/// subtracts this itself. Having the caller compensate instead is what used
/// to push the fourth KPI card off the right edge — the margin value was
/// duplicated as a hand-tuned constant that drifted from the real one.
const KPI_CARD_CHROME: f32 = 24.0;

/// KPI card: uppercase label, big tabular value, weak detail line, and a
/// 3px accent stripe down the left edge in `stripe`.
///
/// `outer_width` is the full slot the card occupies, borders included.
pub fn kpi_card(
    ui: &mut egui::Ui,
    outer_width: f32,
    label: &str,
    value: &str,
    detail: &str,
    stripe: Color32,
) {
    let resp = egui::Frame::new()
        .fill(card_fill(ui))
        .stroke(Stroke::new(
            1.0_f32,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .inner_margin(egui::Margin {
            left: 12,
            right: 12,
            top: 10,
            bottom: 10,
        })
        .corner_radius(CornerRadius::same(4))
        .show(ui, |ui| {
            // A row laying these out zeroes item_spacing.x to keep its width
            // arithmetic exact; child uis inherit it, so restore the theme
            // default for the card's own contents.
            ui.spacing_mut().item_spacing.x = ui.ctx().global_style().spacing.item_spacing.x;
            ui.set_width((outer_width - KPI_CARD_CHROME).max(0.0));
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(label.to_uppercase())
                        .size(tokens::FONT_SMALL)
                        .color(ui.visuals().weak_text_color()),
                );
                ui.label(bold(ui, value, tokens::FONT_KPI));
                ui.label(
                    egui::RichText::new(detail)
                        .size(tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });
    // Left accent stripe, painted on the card's own rect so it lands inside
    // the border rather than in the gap before the next card.
    let card = resp.response.rect;
    let stripe_rect = egui::Rect::from_min_size(card.min, egui::vec2(3.0, card.height()));
    ui.painter().rect_filled(
        stripe_rect,
        CornerRadius {
            nw: 4,
            sw: 4,
            ne: 0,
            se: 0,
        },
        stripe,
    );
}

/// Bottom action bar for settings-style tabs: dirty indicator on the left,
/// Discard + Apply on the right. Returns (discard_clicked, apply_clicked).
pub fn apply_bar(ui: &mut egui::Ui, dirty: bool) -> (bool, bool) {
    let s = sem(ui);
    let mut discard = false;
    let mut apply = false;
    ui.add_space(tokens::SPACE_S);
    ui.separator();
    ui.horizontal(|ui| {
        if dirty {
            ui.colored_label(s.warning, "● Unsaved changes");
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let apply_btn = egui::Button::new(
                egui::RichText::new("Apply changes")
                    .color(s.on_accent)
                    .strong(),
            )
            .fill(s.accent);
            if ui.add_enabled(dirty, apply_btn).clicked() {
                apply = true;
            }
            if ui
                .add_enabled(dirty, egui::Button::new("Discard"))
                .clicked()
            {
                discard = true;
            }
        });
    });
    (discard, apply)
}

/// Two-column form row: fixed-width left-aligned label, then the control.
///
/// `label_w` is explicit because the 220px column that suits the full-width
/// Settings pane eats a third of a 560px dialog. What carries across the app
/// is that labels share a column edge, not the specific number.
pub fn form_row_w(
    ui: &mut egui::Ui,
    label_w: f32,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let h = ui.spacing().interact_size.y;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(label_w, h), egui::Sense::hover());
        ui.painter().text(
            egui::pos2(rect.left(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(tokens::FONT_BODY),
            ui.visuals().text_color(),
        );
        add_contents(ui);
    });
    ui.add_space(tokens::SPACE_XS);
}

/// Group heading inside a form: small caps-ish weak title with an optional
/// plain-weight note after it, over a separator.
pub fn group_heading(ui: &mut egui::Ui, title: &str, note: &str) {
    ui.add_space(tokens::SPACE_XS);
    ui.separator();
    ui.add_space(tokens::SPACE_XS);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(
            egui::RichText::new(title.to_uppercase())
                .font(bold_font(tokens::FONT_LABEL))
                .color(ui.visuals().weak_text_color()),
        );
        if !note.is_empty() {
            ui.label(
                egui::RichText::new(note)
                    .size(tokens::FONT_LABEL)
                    .color(ui.visuals().weak_text_color()),
            );
        }
    });
    ui.add_space(tokens::SPACE_XS);
}

/// Card frame around a plot or a grid of cells, sized to an outer box.
///
/// Takes the *outer* width and height and subtracts its own chrome, so
/// callers divide up the row without restating the margin — the same
/// contract as `kpi_card`.
pub fn plot_card(
    ui: &mut egui::Ui,
    outer_w: f32,
    outer_h: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    const CHROME: f32 = 16.0; // 8px inner margin on both sides
    let inner_w = (outer_w - CHROME).max(0.0);
    let inner_h = (outer_h - CHROME).max(0.0);
    egui::Frame::new()
        .fill(card_fill(ui))
        .stroke(egui::Stroke::new(
            1.0_f32,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = ui.ctx().global_style().spacing.item_spacing.x;
            // Both bounds, not just the minimum. Content that sizes itself
            // from available_height() — the CPU history plot does — otherwise
            // reads the whole remaining panel height and swallows everything
            // below the card.
            ui.set_min_size(egui::vec2(inner_w, inner_h));
            ui.set_max_size(egui::vec2(inner_w, inner_h));
            add_contents(ui);
        });
}

/// Column-header label: weak, small — never accent blue, which reads as
/// "interactive/selected". Only the active sort column gets strong text.
pub fn header_text(ui: &egui::Ui, label: &str, active: bool) -> egui::RichText {
    if active {
        egui::RichText::new(label)
            .font(bold_font(tokens::FONT_LABEL))
            .color(strong_color(ui))
    } else {
        egui::RichText::new(label)
            .size(tokens::FONT_LABEL)
            .color(ui.visuals().weak_text_color())
    }
}

/// Monospace font for numeric cells — prevents jitter on live refresh.
pub fn num_font(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

/// QGroupBox-style bordered card with a top-left title — THE section container
/// for every tab (single definition; per-tab copies are deprecated).

/// A bordered container with no heading (used for hero cards where the
/// title is replaced by a large primary value/status).
pub fn card_untitled(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    egui::Frame::new()
        .fill(card_fill(ui))
        .stroke(egui::Stroke::new(1.0_f32, border_color))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
}

pub fn card(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    card_hinted(ui, title, "", add_contents)
}

/// A card whose title carries a weak hint on the same line, e.g.
/// "Active cores in Gaming Mode — click to park or activate".
pub fn card_hinted(
    ui: &mut egui::Ui,
    title: &str,
    hint: &str,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    egui::Frame::new()
        .fill(card_fill(ui))
        .stroke(egui::Stroke::new(1.0_f32, border_color))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(bold(ui, title, tokens::FONT_HEADING));
                if !hint.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("— {hint}"))
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });
            ui.add_space(tokens::SPACE_XS);
            add_contents(ui);
        });
}

/// Full-width alert/notice banner with semantic colour and an optional action
/// button. Returns true if the action button was clicked.
pub fn banner(ui: &mut egui::Ui, color: Color32, text: &str, action: Option<&str>) -> bool {
    banner_dismissible(ui, color, text, action, false).0
}

/// Banner with an optional ✕ that closes it. Returns
/// (action_clicked, dismiss_clicked).
pub fn banner_dismissible(
    ui: &mut egui::Ui,
    color: Color32,
    text: &str,
    action: Option<&str>,
    dismissible: bool,
) -> (bool, bool) {
    let mut clicked = false;
    let mut dismissed = false;
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            26,
        ))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.colored_label(color, text);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right-to-left: the ✕ is added first so it sits outermost,
                    // past the primary action.
                    if dismissible
                        && ui
                            .add(egui::Button::new("✕").frame(false))
                            .on_hover_text("Dismiss")
                            .clicked()
                    {
                        dismissed = true;
                    }
                    if let Some(label) = action {
                        let btn = egui::Button::new(
                            egui::RichText::new(label)
                                .color(sem(ui).on_accent)
                                .font(bold_font(tokens::FONT_BODY)),
                        )
                        .fill(color);
                        if ui.add(btn).clicked() {
                            clicked = true;
                        }
                    }
                });
            });
        });
    (clicked, dismissed)
}

// ── Semantic colour system ────────────────────────────────────────────────────
//
// ONE source of truth for every semantic colour in the app. Tabs must never
// hardcode rgb literals — call `sem(ui)` (or the ramp below) so Breeze Light
// automatically gets its darker, AA-contrast variants.

/// Semantic palette resolved for the active theme.
#[derive(Debug, Clone, Copy)]
pub struct Sem {
    /// Healthy / low load / success
    pub ok: Color32,
    /// Mid load (≥50%)
    pub mid: Color32,
    /// Elevated / attention (≥70%)
    pub warning: Color32,
    /// Critical / destructive (≥85%)
    pub negative: Color32,
    /// Accent + selection (also "rules"/"network" info blue)
    pub accent: Color32,
    /// Readable text colour on top of an accent-filled surface
    pub on_accent: Color32,
    /// Manual/user actions (log category, network TX)
    pub manual: Color32,
}

/// Semantic palette for the active theme (dark vs. Breeze Light).
pub fn sem(ui: &egui::Ui) -> Sem {
    sem_for(ui.visuals().dark_mode)
}

/// Semantic palette for an explicit theme mode.
pub fn sem_for(dark_mode: bool) -> Sem {
    if dark_mode {
        Sem {
            ok: Color32::from_rgb(0x27, 0xae, 0x60),
            mid: Color32::from_rgb(0x8b, 0xc3, 0x4a),
            warning: Color32::from_rgb(0xf6, 0x74, 0x00),
            negative: Color32::from_rgb(0xda, 0x44, 0x53),
            accent: Color32::from_rgb(0x3d, 0xae, 0xe9),
            on_accent: Color32::from_rgb(0x0d, 0x14, 0x18),
            manual: Color32::from_rgb(0x9b, 0x59, 0xb6),
        }
    } else {
        // Breeze Light: accent darkens (#3daee9 has too little contrast on
        // white) and the semantic ramp darkens for AA contrast on light fills.
        Sem {
            ok: Color32::from_rgb(0x1e, 0x84, 0x49),
            mid: Color32::from_rgb(0x68, 0x9f, 0x38),
            warning: Color32::from_rgb(0xc0, 0x50, 0x00),
            negative: Color32::from_rgb(0xc0, 0x39, 0x2b),
            accent: Color32::from_rgb(0x29, 0x80, 0xb9),
            on_accent: Color32::WHITE,
            manual: Color32::from_rgb(0x7d, 0x3c, 0x98),
        }
    }
}

/// Same colour at a given alpha — for chip fills, heat strips, badges.
pub fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

// ── CPU load colour ramp (Breeze semantic colours) ────────────────────────────

/// Load colour for `pct` ∈ [0,100] in the ACTIVE theme. Use this for bars,
/// table values, sparklines and graphs so one ramp drives everything.
pub fn load_color(ui: &egui::Ui, pct: f32) -> Color32 {
    load_color_for(ui.visuals().dark_mode, pct)
}

/// Load colour for an explicit theme mode.
pub fn load_color_for(dark_mode: bool, pct: f32) -> Color32 {
    let s = sem_for(dark_mode);
    let stops = [
        (0.0, s.ok),
        (50.0, s.mid),
        (70.0, s.warning),
        (85.0, s.negative),
        (100.0, s.negative),
    ];
    let pct = pct.clamp(0.0, 100.0);
    for i in 0..stops.len() - 1 {
        let (p0, c0) = stops[i];
        let (p1, c1) = stops[i + 1];
        if pct <= p1 {
            let t = if (p1 - p0).abs() > f32::EPSILON {
                (pct - p0) / (p1 - p0)
            } else {
                0.0
            };
            let lerp = |a: u8, b: u8| (a as f32 + t * (b as f32 - a as f32)).round() as u8;
            return Color32::from_rgb(
                lerp(c0.r(), c1.r()),
                lerp(c0.g(), c1.g()),
                lerp(c0.b(), c1.b()),
            );
        }
    }
    s.negative
}

/// Dark-theme ramp kept for painters that have no `Ui` at hand.
pub fn cpu_load_color(pct: f32) -> Color32 {
    const RAMP: &[(f32, u8, u8, u8)] = &[
        (0.0, 0x27, 0xae, 0x60),   // Breeze green  (#27ae60)
        (50.0, 0x8b, 0xc3, 0x4a),  // mid green-yellow
        (70.0, 0xf6, 0x74, 0x00),  // Breeze orange (#f67400)
        (85.0, 0xda, 0x44, 0x53),  // Breeze red    (#da4453)
        (100.0, 0xb0, 0x20, 0x2e), // deep red
    ];

    let pct = pct.clamp(0.0, 100.0);
    for i in 0..RAMP.len() - 1 {
        let (p0, r0, g0, b0) = RAMP[i];
        let (p1, r1, g1, b1) = RAMP[i + 1];
        if pct <= p1 {
            let t = if (p1 - p0).abs() > f32::EPSILON {
                (pct - p0) / (p1 - p0)
            } else {
                0.0
            };
            let lerp =
                |a: u8, b: u8| -> u8 { (a as f32 + t * (b as f32 - a as f32)).round() as u8 };
            return Color32::from_rgb(lerp(r0, r1), lerp(g0, g1), lerp(b0, b1));
        }
    }
    let (_, r, g, b) = *RAMP.last().unwrap();
    Color32::from_rgb(r, g, b)
}

// ── Row highlight colours for the process table ───────────────────────────────

// ── Theme application ─────────────────────────────────────────────────────────

/// Scroll bars that are visible without hovering them.
///
/// egui's `solid()` preset sets `dormant_handle_opacity` to 0.0, so the handle
/// is fully transparent until the pointer enters the scroll area. A panel
/// whose content overflows then looks truncated rather than scrollable —
/// which is exactly how the Settings tab's last card and the rule dialog's
/// Nice/I-O rows read.
///
/// `foreground_color` takes the handle from the text colour rather than
/// `widgets.*.bg_fill`, so this needs no palette change; bg_fill is shared
/// with checkbox and slider backgrounds and is the wrong knob for this.
fn scroll_style() -> egui::style::ScrollStyle {
    let mut s = egui::style::ScrollStyle::solid();
    s.bar_width = 10.0;
    s.foreground_color = true;
    s.dormant_handle_opacity = 0.32;
    s.active_handle_opacity = 0.55;
    s.interact_handle_opacity = 0.85;
    s.dormant_background_opacity = 0.12;
    s
}

pub fn apply(ctx: &Context, native_ppp: f32) {
    // Ensure the rendering scale matches the display's native DPI so fonts
    // don't shrink when the theme is reapplied (e.g. after toggling system theme).
    ctx.set_pixels_per_point(native_ppp);
    let mut style = Style::default();

    let mut vis = Visuals::dark();

    // ── Backgrounds ──────────────────────────────────────────────────────
    vis.window_fill = Breeze::WINDOW_BG;
    vis.panel_fill = Breeze::WINDOW_BG;
    vis.faint_bg_color = Breeze::ALT_BASE;
    vis.extreme_bg_color = Breeze::BASE;

    // ── Text ─────────────────────────────────────────────────────────────
    vis.override_text_color = Some(Breeze::TEXT);

    // ── Widgets ──────────────────────────────────────────────────────────
    let rounding = CornerRadius::same(4);

    // non-interactive (labels, separators)
    vis.widgets.noninteractive.bg_fill = Breeze::WINDOW_BG;
    vis.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, Breeze::BORDER);
    vis.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Breeze::BORDER);
    vis.widgets.noninteractive.corner_radius = rounding;

    // inactive (buttons, checkboxes at rest)
    vis.widgets.inactive.bg_fill = Breeze::BUTTON_BG;
    vis.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Breeze::BORDER);
    vis.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, Breeze::TEXT);
    vis.widgets.inactive.corner_radius = rounding;

    // hovered
    vis.widgets.hovered.bg_fill = Breeze::BUTTON_HOVER;
    vis.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Breeze::HIGHLIGHT);
    vis.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, Breeze::TEXT);
    vis.widgets.hovered.corner_radius = rounding;

    // active (pressed)
    vis.widgets.active.bg_fill = Breeze::HIGHLIGHT;
    vis.widgets.active.bg_stroke = Stroke::new(1.0_f32, Breeze::HIGHLIGHT);
    vis.widgets.active.fg_stroke = Stroke::new(1.5_f32, Breeze::TEXT);
    vis.widgets.active.corner_radius = rounding;

    // open (combo boxes, menus)
    vis.widgets.open.bg_fill = Breeze::ALT_BASE;
    vis.widgets.open.bg_stroke = Stroke::new(1.0_f32, Breeze::HIGHLIGHT);
    vis.widgets.open.fg_stroke = Stroke::new(1.0_f32, Breeze::TEXT);
    vis.widgets.open.corner_radius = rounding;

    // ── Selection ────────────────────────────────────────────────────────
    vis.selection.bg_fill = Color32::from_rgba_unmultiplied(0x3d, 0xae, 0xe9, 0x66); // 40% alpha
    vis.selection.stroke = Stroke::new(1.0_f32, Breeze::HIGHLIGHT);

    // ── Misc ─────────────────────────────────────────────────────────────
    vis.hyperlink_color = Breeze::LINK;
    vis.window_stroke = Stroke::new(1.0_f32, Breeze::BORDER);
    vis.window_shadow = egui::epaint::Shadow::NONE;
    vis.window_corner_radius = CornerRadius::same(4);

    // Striped table alternate row colour
    vis.faint_bg_color = Breeze::ALT_BASE;

    style.visuals = vis;

    // ── Typography ───────────────────────────────────────────────────────
    style.text_styles = {
        use egui::TextStyle::*;
        [
            (Small, FontId::proportional(12.0)),
            (Body, FontId::proportional(14.0)),
            (Button, FontId::proportional(14.0)),
            (Heading, FontId::proportional(16.0)),
            (Monospace, FontId::monospace(13.0)),
        ]
        .into()
    };

    // ── Spacing — comfortable row height ─────────────────────────────────
    style.spacing.interact_size.y = 24.0;
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.scroll = scroll_style();

    // ── Striped table — more visible alt row ─────────────────────────────
    // faint_bg_color is set in vis above; override again after style.visuals assignment
    // to ensure it survives (vis already set above, so we patch style.visuals here)
    style.visuals.faint_bg_color = Color32::from_rgb(0x3d, 0x41, 0x47); // visibly lighter than WINDOW_BG

    ctx.set_global_style(style);
}

// ── Breeze Light theme ────────────────────────────────────────────────────────

pub fn apply_light(ctx: &Context, native_ppp: f32) {
    ctx.set_pixels_per_point(native_ppp);
    let mut style = Style::default();

    let mut vis = Visuals::light();

    // Backgrounds
    let window_bg = Color32::from_rgb(0xef, 0xf0, 0xf1); // #eff0f1
    let base = Color32::from_rgb(0xff, 0xff, 0xff); // #ffffff  table row even
    let alt_base = Color32::from_rgb(0xf4, 0xf4, 0xf4); // #f4f4f4  table row odd
    let tab_bar = Color32::from_rgb(0xd5, 0xd9, 0xde); // #d5d9de  tab bar / panel bg

    vis.window_fill = window_bg;
    vis.panel_fill = tab_bar;
    vis.faint_bg_color = alt_base;
    vis.extreme_bg_color = base;

    // Text
    let text = Color32::from_rgb(0x31, 0x36, 0x3b); // #31363b
    vis.override_text_color = Some(text);

    // Borders / buttons
    let border = Color32::from_rgb(0xb0, 0xb8, 0xc0); // #b0b8c0 — sharper than before
    let button_bg = Color32::from_rgb(0xef, 0xf0, 0xf1);
    let button_hov = Color32::from_rgb(0xe0, 0xe4, 0xe8);
    let button_dis = Color32::from_rgb(0xdd, 0xe1, 0xe5); // #dde1e5 disabled bg
    let text_dis = Color32::from_rgb(0xa0, 0xa8, 0xb0); // #a0a8b0 disabled fg
    let highlight = Color32::from_rgb(0x3d, 0xae, 0xe9); // #3daee9 Breeze blue

    let rounding = CornerRadius::same(4);

    vis.widgets.noninteractive.bg_fill = window_bg;
    vis.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.noninteractive.corner_radius = rounding;

    vis.widgets.inactive.bg_fill = button_bg;
    vis.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.inactive.corner_radius = rounding;

    vis.widgets.hovered.bg_fill = button_hov;
    vis.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, highlight);
    vis.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.hovered.corner_radius = rounding;

    // Active (pressed) — Breeze blue fill, white text
    vis.widgets.active.bg_fill = highlight;
    vis.widgets.active.bg_stroke = Stroke::new(1.0_f32, highlight);
    vis.widgets.active.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
    vis.widgets.active.corner_radius = rounding;

    // Open (combo / menu dropdown) — slightly darker base
    vis.widgets.open.bg_fill = button_hov;
    vis.widgets.open.bg_stroke = Stroke::new(1.0_f32, highlight);
    vis.widgets.open.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.open.corner_radius = rounding;

    // Selection — Breeze blue at ~31% alpha (premultiplied: 61,174,233 * 80/255 ≈ 19,55,73)
    vis.selection.bg_fill = Color32::from_rgba_premultiplied(19, 55, 73, 80);
    vis.selection.stroke = Stroke::new(1.0_f32, highlight);

    vis.hyperlink_color = Color32::from_rgb(0x29, 0x80, 0xb9);
    vis.window_stroke = Stroke::new(1.0_f32, border);
    vis.window_shadow = egui::epaint::Shadow::NONE;
    vis.window_corner_radius = CornerRadius::same(4);

    style.visuals = vis;

    style.text_styles = {
        use egui::TextStyle::*;
        [
            (Small, FontId::proportional(12.0)),
            (Body, FontId::proportional(14.0)),
            (Button, FontId::proportional(14.0)),
            (Heading, FontId::proportional(16.0)),
            (Monospace, FontId::monospace(13.0)),
        ]
        .into()
    };

    style.spacing.interact_size.y = 24.0;
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.scroll = scroll_style();
    // Striped table alternate row — explicitly set after visuals assignment
    style.visuals.faint_bg_color = alt_base;

    ctx.set_global_style(style);

    let _ = (button_dis, text_dis); // available for disabled-widget callers
}

// ── Adwaita themes (GNOME) ────────────────────────────────────────────────────
//
// Palette from libadwaita's named colours, so the window blends into a GNOME
// session instead of standing out as a KDE transplant. The two modes share
// structure and differ only in colour values, hence one parameterised fn.

pub fn apply_adwaita(ctx: &Context, native_ppp: f32, dark: bool) {
    ctx.set_pixels_per_point(native_ppp);
    let mut style = Style::default();

    let mut vis = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    // libadwaita: window_bg_color / view_bg_color / headerbar shades.
    let (window_bg, base, alt_base, panel, text, border, button_bg, button_hov, accent, link) =
        if dark {
            (
                Color32::from_rgb(0x24, 0x24, 0x24), // window_bg_color
                Color32::from_rgb(0x1e, 0x1e, 0x1e), // view_bg_color
                Color32::from_rgb(0x2c, 0x2c, 0x2c), // striped row
                Color32::from_rgb(0x24, 0x24, 0x24),
                Color32::WHITE,
                Color32::from_rgb(0x45, 0x45, 0x45), // ~15% white border
                Color32::from_rgb(0x35, 0x35, 0x35), // button: 8% white on window
                Color32::from_rgb(0x40, 0x40, 0x40), // hover: ~12% white
                Color32::from_rgb(0x35, 0x84, 0xe4), // accent_bg_color blue 3
                Color32::from_rgb(0x78, 0xae, 0xed), // accent_color on dark
            )
        } else {
            (
                Color32::from_rgb(0xfa, 0xfa, 0xfa), // window_bg_color
                Color32::from_rgb(0xff, 0xff, 0xff), // view_bg_color
                Color32::from_rgb(0xf2, 0xf2, 0xf2), // striped row
                Color32::from_rgb(0xeb, 0xeb, 0xeb), // headerbar shade
                Color32::from_rgb(0x33, 0x33, 0x33), // ~80% black
                Color32::from_rgb(0xc9, 0xc9, 0xc9), // ~15% black border
                Color32::from_rgb(0xff, 0xff, 0xff),
                Color32::from_rgb(0xef, 0xef, 0xef),
                Color32::from_rgb(0x35, 0x84, 0xe4), // accent_bg_color blue 3
                Color32::from_rgb(0x1c, 0x71, 0xd8), // accent_color on light
            )
        };

    vis.window_fill = window_bg;
    vis.panel_fill = panel;
    vis.faint_bg_color = alt_base;
    vis.extreme_bg_color = base;

    vis.override_text_color = Some(text);

    // Adwaita rounds corners more generously than Breeze.
    let rounding = CornerRadius::same(6);

    vis.widgets.noninteractive.bg_fill = window_bg;
    vis.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.noninteractive.corner_radius = rounding;

    vis.widgets.inactive.bg_fill = button_bg;
    vis.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, border);
    vis.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.inactive.corner_radius = rounding;

    vis.widgets.hovered.bg_fill = button_hov;
    vis.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, accent);
    vis.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.hovered.corner_radius = rounding;

    // Pressed: accent fill; white text reads on #3584e4 in both modes.
    vis.widgets.active.bg_fill = accent;
    vis.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent);
    vis.widgets.active.fg_stroke = Stroke::new(1.5_f32, Color32::WHITE);
    vis.widgets.active.corner_radius = rounding;

    vis.widgets.open.bg_fill = button_hov;
    vis.widgets.open.bg_stroke = Stroke::new(1.0_f32, accent);
    vis.widgets.open.fg_stroke = Stroke::new(1.0_f32, text);
    vis.widgets.open.corner_radius = rounding;

    vis.selection.bg_fill = tint(accent, 0x66);
    vis.selection.stroke = Stroke::new(1.0_f32, accent);

    vis.hyperlink_color = link;
    vis.window_stroke = Stroke::new(1.0_f32, border);
    vis.window_shadow = egui::epaint::Shadow::NONE;
    vis.window_corner_radius = CornerRadius::same(6);

    style.visuals = vis;

    style.text_styles = {
        use egui::TextStyle::*;
        [
            (Small, FontId::proportional(12.0)),
            (Body, FontId::proportional(14.0)),
            (Button, FontId::proportional(14.0)),
            (Heading, FontId::proportional(16.0)),
            (Monospace, FontId::monospace(13.0)),
        ]
        .into()
    };

    style.spacing.interact_size.y = 24.0;
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.scroll = scroll_style();
    style.visuals.faint_bg_color = alt_base;

    ctx.set_global_style(style);
}
