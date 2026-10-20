use eframe::egui::Ui;

use crate::config::CodecName;
use crate::settings::SettingsApp;
use crate::settings::widgets::{self, Look, hint, row};

const FPS_PRESETS: [u32; 6] = [0, 30, 60, 120, 144, 240];
const COUNTDOWN_PRESETS: [u32; 3] = [0, 3, 5];

fn fps_label(fps: u32) -> String {
    match fps {
        0 => "native".into(),
        n => n.to_string(),
    }
}

fn quality_word(q: u32) -> &'static str {
    match q {
        0..=14 => "near lossless",
        15..=20 => "very sharp",
        21..=24 => "sharp",
        25..=29 => "balanced",
        _ => "small files",
    }
}

fn effort_word(e: u32) -> &'static str {
    match e {
        0..=24 => "fastest, least gpu load",
        25..=49 => "fast",
        50..=69 => "balanced",
        _ => "best compression, more gpu load",
    }
}

impl SettingsApp {
    pub(in crate::settings) fn recording(&mut self, ui: &mut Ui, look: &Look) {
        let t3 = look.p.t3;
        let mut presets = FPS_PRESETS.to_vec();
        if !presets.contains(&self.cfg.fps) {
            presets.push(self.cfg.fps); // a hand-edited value stays selectable
            presets.sort_unstable();
        }
        let labels: Vec<String> = presets.iter().map(|&f| fps_label(f)).collect();
        let fps: Vec<(u32, &str)> = presets.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "frame rate", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.fps, &fps, None);
            let text = match self.cfg.fps {
                0 => "native follows each monitor's refresh rate".to_string(),
                n => format!("{n} fps, or less on slower monitors"),
            };
            hint(ui, text, t3);
        });
        row(ui, look, "codec", 7.0, |ui| {
            let codecs = [(CodecName::H264, "h.264"), (CodecName::Hevc, "hevc")];
            widgets::segmented(ui, look, &mut self.cfg.codec, &codecs, Some(168.0));
            hint(ui, "h.264 — plays everywhere · hevc — ~30 % smaller files", t3);
        });
        row(ui, look, "quality", 0.0, |ui| {
            let word = quality_word(self.cfg.quality);
            widgets::slider(ui, look, &mut self.cfg.quality, 12..=36, word, "12 ← sharper, bigger", "smaller, softer → 36");
        });
        row(ui, look, "encoder effort", 0.0, |ui| {
            let word = effort_word(self.cfg.effort);
            widgets::slider(ui, look, &mut self.cfg.effort, 0..=100, word, "0 ← faster", "smaller files → 100");
        });
        row(ui, look, "desktop audio", 1.0, |ui| {
            let text = if self.cfg.audio_desktop { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.audio_desktop, text);
            hint(ui, "what your default output device plays", t3);
        });
        row(ui, look, "microphone", 1.0, |ui| {
            let text = if self.cfg.audio_mic { "on" } else { "off" };
            widgets::toggle(ui, look, &mut self.cfg.audio_mic, text);
            hint(ui, "your default recording device; with desktop audio, both go into one track", t3);
        });
        let mut counts = COUNTDOWN_PRESETS.to_vec();
        if !counts.contains(&self.cfg.countdown) {
            counts.push(self.cfg.countdown);
            counts.sort_unstable();
        }
        let labels: Vec<String> = counts.iter().map(|&s| if s == 0 { "off".into() } else { format!("{s} s") }).collect();
        let choices: Vec<(u32, &str)> = counts.iter().copied().zip(labels.iter().map(String::as_str)).collect();
        row(ui, look, "countdown", 7.0, |ui| {
            widgets::segmented(ui, look, &mut self.cfg.countdown, &choices, Some(200.0));
            hint(ui, "esc or the record hotkey cancels it", t3);
        });
        row(ui, look, "mouse cursor", 1.0, |ui| {
            let text = if self.cfg.draw_cursor { "shown in captures" } else { "hidden" };
            widgets::toggle(ui, look, &mut self.cfg.draw_cursor, text);
        });
        row(ui, look, "trimmer", 1.0, |ui| {
            widgets::toggle(ui, look, &mut self.cfg.open_trimmer, "open trimmer after recording");
        });
    }
}
