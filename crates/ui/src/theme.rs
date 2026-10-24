use std::path::Path;
use std::sync::Arc;

use eframe::egui::{self, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, TextStyle, vec2};

use crate::widgets::{self, medium, mono, sans};

/// IBM Plex Sans and Mono, unmodified (SIL Open Font License, see assets/fonts/OFL.txt).
const FONTS: [(&str, &[u8]); 5] = [
    ("sans", include_bytes!("../../../assets/fonts/IBMPlexSans-Regular.ttf")),
    ("sans-medium", include_bytes!("../../../assets/fonts/IBMPlexSans-Medium.ttf")),
    ("sans-semibold", include_bytes!("../../../assets/fonts/IBMPlexSans-SemiBold.ttf")),
    ("mono", include_bytes!("../../../assets/fonts/IBMPlexMono-Regular.ttf")),
    ("mono-medium", include_bytes!("../../../assets/fonts/IBMPlexMono-Medium.ttf")),
];
pub const FONT_LICENSE: &str = include_str!("../../../assets/fonts/OFL.txt");

pub fn fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let defaults = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, bytes) in FONTS {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(bytes)));
    }
    // Segoe UI covers what Plex doesn't (other scripts in folder names, for instance), Segoe UI
    // Symbol the symbols neither has (the ♪ by apps that play sound).
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    let mut fallback: Vec<String> = Vec::new();
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-symbol", "seguisym.ttf")] {
        if let Ok(bytes) = std::fs::read(Path::new(&windir).join("Fonts").join(file)) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
            fallback.push(name.into());
        }
    }
    fallback.extend(defaults);
    let families = [
        (FontFamily::Proportional, "sans"),
        (widgets::MEDIUM.clone(), "sans-medium"),
        (widgets::SEMIBOLD.clone(), "sans-semibold"),
        (FontFamily::Monospace, "mono"),
        (widgets::MONO_MEDIUM.clone(), "mono-medium"),
    ];
    for (family, first) in families {
        let chain = std::iter::once(first.to_string()).chain(fallback.iter().cloned()).collect();
        fonts.families.insert(family, chain);
    }
    ctx.set_fonts(fonts);
    ctx.all_styles_mut(|s| {
        s.interaction.selectable_labels = false;
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.scroll = egui::style::ScrollStyle {
            bar_width: 6.0,
            floating_width: 6.0,
            handle_min_length: 24.0,
            foreground_color: false,
            dormant_background_opacity: 0.0,
            active_background_opacity: 0.0,
            interact_background_opacity: 0.0,
            dormant_handle_opacity: 1.0,
            active_handle_opacity: 1.0,
            interact_handle_opacity: 1.0,
            ..egui::style::ScrollStyle::floating()
        };
        s.text_styles.insert(TextStyle::Body, sans(13.0));
        s.text_styles.insert(TextStyle::Button, medium(13.0));
        s.text_styles.insert(TextStyle::Small, sans(12.0));
        s.text_styles.insert(TextStyle::Monospace, mono(12.0));
    });
}

/// egui's own widgets (text fields, scroll bars) in the palette's colours.
pub fn visuals(ctx: &egui::Context, p: &widgets::Palette, light: bool) {
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = p.surf;
    v.window_fill = p.surf;
    v.extreme_bg_color = p.field;
    v.selection.bg_fill = p.acc.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, p.t1);
    v.text_cursor.stroke = Stroke::new(1.5, p.t1);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
        w.fg_stroke.color = p.t1;
    }
    // scroll handles are painted in these fills
    for (w, fill) in [(&mut v.widgets.inactive, p.line2), (&mut v.widgets.hovered, p.line3), (&mut v.widgets.active, p.t3)] {
        w.bg_fill = fill;
        w.corner_radius = CornerRadius::same(3);
    }
    ctx.set_theme(if light { egui::Theme::Light } else { egui::Theme::Dark });
    ctx.set_visuals(v);
}
