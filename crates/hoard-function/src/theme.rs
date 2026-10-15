use egui::Color32;
use serde::{Deserialize, Serialize};
use stable_ui::{Rgba8, Theme as SharedTheme};
use std::{fs, path::Path};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeFile {
    #[serde(default)]
    pub colors: ThemeColorsFile,
    #[serde(default)]
    pub metrics: ThemeMetrics,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThemeColorsFile {
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub panel: Option<String>,
    #[serde(default)]
    pub panel_alt: Option<String>,
    #[serde(default)]
    pub sidebar: Option<String>,
    #[serde(default)]
    pub foreground: Option<String>,
    #[serde(default)]
    pub muted: Option<String>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub accent_soft: Option<String>,
    #[serde(default)]
    pub accent_green: Option<String>,
    #[serde(default)]
    pub selection: Option<String>,
    #[serde(default)]
    pub selection_text: Option<String>,
    #[serde(default)]
    pub border: Option<String>,
    #[serde(default)]
    pub danger: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeMetrics {
    #[serde(default)]
    pub rounding: Option<f32>,
    #[serde(default = "default_row_height")]
    pub row_height: f32,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    #[serde(default = "default_preview_width")]
    pub preview_width: f32,
}
fn default_row_height() -> f32 {
    36.0
}
fn default_sidebar_width() -> f32 {
    240.0
}
fn default_preview_width() -> f32 {
    300.0
}
impl Default for ThemeMetrics {
    fn default() -> Self {
        Self {
            rounding: None,
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

fn c(v: Rgba8) -> Color32 {
    Color32::from_rgba_unmultiplied(v.0, v.1, v.2, v.3)
}
fn alpha(v: Color32, a: u8) -> Color32 {
    let [r, g, b, _] = v.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

impl Default for Theme {
    fn default() -> Self {
        let shared = SharedTheme::default();
        let accent = c(shared.accent);
        Self {
            background: c(shared.background),
            panel: c(shared.surface),
            panel_alt: c(shared.surface_alt),
            sidebar: c(shared.background),
            foreground: c(shared.text),
            muted: c(shared.text_muted),
            accent,
            accent_soft: alpha(accent, 110),
            accent_green: c(shared.active),
            selection: alpha(accent, 210),
            selection_text: c(shared.text),
            border: c(shared.border),
            danger: c(shared.danger),
            metrics: ThemeMetrics {
                rounding: Some(shared.metrics.corner_radius),
                ..ThemeMetrics::default()
            },
        }
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
        let mut t = Self::default();
        macro_rules! apply {
            ($field:ident) => {
                if let Some(ref raw) = f.colors.$field {
                    t.$field = parse_color(raw, t.$field);
                }
            };
        }
        apply!(background);
        apply!(panel);
        apply!(panel_alt);
        apply!(sidebar);
        apply!(foreground);
        apply!(muted);
        apply!(accent);
        apply!(accent_soft);
        apply!(accent_green);
        apply!(selection);
        apply!(selection_text);
        apply!(border);
        apply!(danger);
        t.metrics.row_height = f.metrics.row_height;
        t.metrics.sidebar_width = f.metrics.sidebar_width;
        t.metrics.preview_width = f.metrics.preview_width;
        if f.metrics.rounding.is_some() {
            t.metrics.rounding = f.metrics.rounding;
        }
        t
    }
    pub fn rounding(&self) -> f32 {
        self.metrics
            .rounding
            .unwrap_or_else(|| SharedTheme::default().metrics.corner_radius)
    }
    pub fn with_surface_opacity(mut self, opacity: f32) -> Self {
        let opacity = opacity.clamp(0.55, 1.0);
        let apply = |color: Color32| {
            let [r, g, b, a] = color.to_srgba_unmultiplied();
            Color32::from_rgba_unmultiplied(
                r,
                g,
                b,
                ((a as f32) * opacity).round().clamp(0.0, 255.0) as u8,
            )
        };
        self.background = apply(self.background);
        self.panel = apply(self.panel);
        self.panel_alt = apply(self.panel_alt);
        self.sidebar = apply(self.sidebar);
        self
    }
    pub fn apply_visuals(&self, ctx: &egui::Context) {
        let shared = SharedTheme::default();
        let mut v = egui::Visuals::dark();
        v.panel_fill = self.background;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.sidebar;
        v.faint_bg_color = self.panel_alt;
        v.override_text_color = Some(self.foreground);
        v.selection.bg_fill = self.selection;
        v.selection.stroke = egui::Stroke::new(shared.metrics.border_width, self.accent);
        v.hyperlink_color = self.accent_green;
        v.widgets.inactive.bg_fill = self.panel;
        v.widgets.inactive.weak_bg_fill = self.panel;
        v.widgets.inactive.fg_stroke.color = self.foreground;
        v.widgets.hovered.bg_fill = self.accent_soft;
        v.widgets.hovered.fg_stroke.color = self.foreground;
        v.widgets.hovered.bg_stroke = egui::Stroke::new(shared.metrics.border_width, self.accent);
        v.widgets.active.bg_fill = self.selection;
        v.widgets.active.bg_stroke = egui::Stroke::new(shared.metrics.border_width, self.accent);
        ctx.set_visuals(v);
        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style.spacing.item_spacing = egui::vec2(8.0, shared.metrics.section_spacing - 3.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(21.0));
        ctx.set_style_of(egui::Theme::Dark, style);
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
            parse(&hex[0..2]),
            parse(&hex[2..4]),
            parse(&hex[4..6]),
            parse(&hex[6..8]),
        ) {
            (Some(r), Some(g), Some(b), Some(a)) => Color32::from_rgba_unmultiplied(r, g, b, a),
            _ => fallback,
        },
        _ => fallback,
    }
}
