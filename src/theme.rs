use egui::Color32;
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeFile {
    pub colors: ThemeColorsFile,
    #[serde(default)]
    pub metrics: ThemeMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeColorsFile {
    pub background: String,
    pub panel: String,
    pub panel_alt: String,
    pub sidebar: String,
    pub foreground: String,
    pub muted: String,
    pub accent: String,
    pub accent_soft: String,
    pub accent_green: String,
    pub selection: String,
    pub selection_text: String,
    pub border: String,
    pub danger: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMetrics {
    #[serde(default = "default_rounding")]
    pub rounding: f32,
    #[serde(default = "default_row_height")]
    pub row_height: f32,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_preview_width")]
    pub preview_width: f32,
}

fn default_rounding() -> f32 { 9.0 }
fn default_row_height() -> f32 { 36.0 }
fn default_sidebar_width() -> f32 { 240.0 }
fn default_preview_width() -> f32 { 300.0 }

impl Default for ThemeMetrics {
    fn default() -> Self {
        Self {
            rounding: default_rounding(),
            row_height: default_row_height(),
            sidebar_width: default_sidebar_width(),
            preview_width: default_preview_width(),
        }
    }
}

#[derive(Clone)]
pub struct Theme {
    pub background: Color32,
    pub panel: Color32,
    pub panel_alt: Color32,
    pub sidebar: Color32,
    pub foreground: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_green: Color32,
    pub selection: Color32,
    pub selection_text: Color32,
    pub border: Color32,
    pub danger: Color32,
    pub metrics: ThemeMetrics,
}

impl Default for Theme {
    fn default() -> Self {
        let f: ThemeFile = toml::from_str(include_str!("../config/themes/hoard.toml"))
            .expect("embedded default theme must parse");
        Self::from_file(f)
    }
}

impl Theme {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str::<ThemeFile>(&s).ok())
            .map(Self::from_file)
            .unwrap_or_default()
    }

    fn from_file(f: ThemeFile) -> Self {
        Self {
            background: parse_color(&f.colors.background, Color32::from_rgba_premultiplied(8, 11, 18, 238)),
            panel: parse_color(&f.colors.panel, Color32::from_rgba_premultiplied(14, 17, 26, 235)),
            panel_alt: parse_color(&f.colors.panel_alt, Color32::from_rgba_premultiplied(20, 24, 36, 235)),
            sidebar: parse_color(&f.colors.sidebar, Color32::from_rgba_premultiplied(8, 10, 16, 245)),
            foreground: parse_color(&f.colors.foreground, Color32::from_rgb(235, 235, 244)),
            muted: parse_color(&f.colors.muted, Color32::from_rgb(151, 154, 176)),
            accent: parse_color(&f.colors.accent, Color32::from_rgb(164, 76, 255)),
            accent_soft: parse_color(&f.colors.accent_soft, Color32::from_rgba_premultiplied(112, 52, 168, 110)),
            accent_green: parse_color(&f.colors.accent_green, Color32::from_rgb(57, 255, 136)),
            selection: parse_color(&f.colors.selection, Color32::from_rgba_premultiplied(86, 42, 128, 210)),
            selection_text: parse_color(&f.colors.selection_text, Color32::WHITE),
            border: parse_color(&f.colors.border, Color32::from_rgb(77, 49, 105)),
            danger: parse_color(&f.colors.danger, Color32::from_rgb(255, 82, 99)),
            metrics: f.metrics,
        }
    }


    pub fn with_surface_opacity(mut self, opacity: f32) -> Self {
        let opacity = opacity.clamp(0.55, 1.0);
        let apply = |color: Color32| {
            let [r, g, b, a] = color.to_srgba_unmultiplied();
            let alpha = ((a as f32) * opacity).round().clamp(0.0, 255.0) as u8;
            Color32::from_rgba_unmultiplied(r, g, b, alpha)
        };
        self.background = apply(self.background);
        self.panel = apply(self.panel);
        self.panel_alt = apply(self.panel_alt);
        self.sidebar = apply(self.sidebar);
        self
    }

    pub fn apply_visuals(&self, ctx: &egui::Context) {
        let mut v = egui::Visuals::dark();
        v.panel_fill = self.background;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.sidebar;
        v.faint_bg_color = self.panel_alt;
        v.override_text_color = Some(self.foreground);
        v.selection.bg_fill = self.selection;
        v.selection.stroke = egui::Stroke::new(1.0_f32, self.accent);
        v.hyperlink_color = self.accent_green;
        v.widgets.inactive.bg_fill = self.panel;
        v.widgets.inactive.weak_bg_fill = self.panel;
        v.widgets.inactive.fg_stroke.color = self.foreground;
        v.widgets.hovered.bg_fill = self.accent_soft;
        v.widgets.hovered.fg_stroke.color = self.foreground;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        v.widgets.active.bg_fill = self.selection;
        v.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, self.accent);
        ctx.set_visuals(v);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(21.0));
        ctx.set_style(style);
    }
}

fn parse_color(s: &str, fallback: Color32) -> Color32 {
    let hex = s.trim().trim_start_matches('#');
    let parse = |slice: &str| u8::from_str_radix(slice, 16).ok();
    match hex.len() {
        6 => match (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6])) {
            (Some(r), Some(g), Some(b)) => Color32::from_rgb(r, g, b),
            _ => fallback,
        },
        8 => match (
            parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6]), parse(&hex[6..8]),
        ) {
            (Some(r), Some(g), Some(b), Some(a)) => Color32::from_rgba_unmultiplied(r, g, b, a),
            _ => fallback,
        },
        _ => fallback,
    }
}
