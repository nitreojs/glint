use std::sync::atomic::Ordering;

use eframe::egui::{
    self, Align, Align2, Color32, CursorIcon, Id, Layout, Margin, Rect, Response, RichText, Role, Sense, Shape, Stroke,
    StrokeKind, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use glint_core::clip::clicks::{ClickColor, ClickSize};
use glint_core::clip::keys::KeySize;
use glint_core::clip::crop::Crop;
use glint_core::clip::export::{self, CodecChoice, Compress, Format, Job, MB, TARGETS_MB, Video, VideoJob};
use glint_core::clip::math::{GIF_FPS, GIF_WARN_BYTES, fmt_short};
use glint_core::record::Codec;
use glint_core::record::keys::Position;
use glint_ui::widgets::{self, Look, anim, ease, glide, mix, mono, sans, semibold};

use super::{ClipApp, PAD};

const ROW_GAP: f32 = 14.0;
const SAVE_H: f32 = 44.0;
const MORE_W: f32 = 40.0;
const CLICK_COLORS: [([u8; 3], &str); 3] = [([0xFF, 0xD6, 0x0A], "yellow"), ([0x34, 0xC7, 0x59], "green"), ([0x0A, 0x84, 0xFF], "blue")];
const HEVC_TIP: &str = "hevc makes smaller files, but some pcs need the free “hevc video extensions” to play them";

fn label(ui: &mut Ui, look: &Look, text: &str) {
    ui.label(RichText::new(text).font(sans(12.0)).color(look.p.t3));
    ui.add_space(-2.0);
}

fn gap(ui: &mut Ui) {
    ui.add_space(ROW_GAP - 6.0);
}

/// A rounded choice; the accent wash slides in when chosen.
fn pill(ui: &mut Ui, look: &Look, text: &str, on: bool) -> Response {
    let p = &look.p;
    let g = ui.painter().layout_no_wrap(text.to_string(), sans(12.0), p.t1);
    let (rect, resp) = ui.allocate_exact_size(vec2(g.size().x + 20.0, 26.0), Sense::click());
    let t = ease(anim(ui, resp.id.with("on"), on, 0.2));
    let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
    let fill = mix(p.field, p.acc.gamma_multiply(0.16), t);
    let ring = mix(mix(p.line2, p.line3, h), p.acc, t);
    ui.painter().rect(rect, 13, fill, Stroke::new(1.0, ring), StrokeKind::Inside);
    ui.painter().galley(rect.center() - g.size() / 2.0, g, mix(p.t2, p.t1, t.max(h)));
    resp.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, on, text));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A pill-shaped number field: accent while it holds a value, warning while that value can't be used.
fn pill_field(ui: &mut Ui, look: &Look, text: &mut String, hint: &str, name: &str, width: f32, warn: bool) -> Response {
    let p = &look.p;
    let (r, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    let bg = ui.painter().add(Shape::Noop);
    let edit = egui::TextEdit::singleline(text)
        .frame(egui::Frame::NONE.inner_margin(Margin::symmetric(9, 0)))
        .font(sans(12.0))
        .text_color(p.t1)
        .hint_text(RichText::new(hint).font(sans(12.0)).color(p.t3))
        .align(Align2::LEFT_CENTER)
        .desired_width(width)
        .min_size(vec2(width, 26.0))
        .char_limit(6);
    let resp = ui.put(r, edit);
    ui.ctx().accesskit_node_builder(resp.id, |n| n.set_label(name));
    let f = anim(ui, resp.id.with("foc"), resp.has_focus(), 0.12);
    let on = !text.is_empty();
    let ring = if warn { p.warn } else if on { p.acc } else { mix(p.line2, p.t2, f) };
    let fill = if on && !warn { p.acc.gamma_multiply(0.16) } else { p.field };
    ui.painter().set(bg, egui::epaint::RectShape::new(r, 13, fill, Stroke::new(1.0, ring), StrokeKind::Inside));
    resp
}

fn sound_line(job: &Job, tracks: usize) -> String {
    match job {
        Job::Gif(_) => "no sound (gif)".into(),
        Job::Video(v) if v.tracks.is_empty() => "no sound".into(),
        Job::Video(v) if v.combine => match v.tracks.iter().filter(|t| t.source.is_none()).count() {
            0 => "sound combined into 1 track".into(),
            n => format!("sound combined into 1 track + {n} empty"),
        },
        Job::Video(v) if v.tracks.len() == 1 && v.tracks[0].source.is_some() && tracks == 1 => "with sound".into(),
        Job::Video(v) => format!("{} sound track{}", v.tracks.len(), if v.tracks.len() == 1 { "" } else { "s" }),
    }
}

fn fps_label(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.01 { format!("{fps:.0} fps") } else { format!("{fps:.2} fps") }
}

fn mbps(bps: u32) -> String {
    let m = bps as f64 / 1e6;
    if m >= 10.0 { format!("{m:.0} Mbps") } else { format!("{m:.1} Mbps") }
}

fn codec_name(c: Codec) -> &'static str {
    match c {
        Codec::H264 => "h.264",
        Codec::Hevc => "hevc",
    }
}

impl ClipApp {
    pub(super) fn panel(&mut self, ui: &mut Ui, look: &Look, rect: Rect) {
        let p = look.p;
        ui.painter().vline(rect.left() + 0.5, rect.y_range(), Stroke::new(1.0, p.line));
        let inner = rect.shrink(PAD);
        let save_row = Rect::from_min_max(pos2(inner.left(), inner.bottom() - SAVE_H), inner.max);
        let est = Rect::from_min_max(pos2(inner.left(), save_row.top() - 14.0 - 54.0), pos2(inner.right(), save_row.top() - 14.0));
        let top = Rect::from_min_max(inner.min, pos2(inner.right(), est.top() - ROW_GAP));
        if self.info().is_none() {
            ui.painter().text(inner.left_top(), Align2::LEFT_TOP, "export", semibold(15.0), p.t1);
            return;
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(top).layout(Layout::top_down(Align::Min)));
        egui::ScrollArea::vertical()
            .id_salt("clip-panel-rows")
            .auto_shrink([false, false])
            .max_height(top.height())
            .show(&mut child, |ui| self.rows(ui, look));

        let job = self.job();
        let src = self.source();
        if let (Some(job), Some(src)) = (&job, &src) {
            let bytes = export::estimate(job, src);
            let over = self.saved.export.target_mb.is_some_and(|mb| bytes > mb as u64 * MB)
                || matches!(job, Job::Gif(_)) && bytes > GIF_WARN_BYTES;
            let color = mix(p.t1, p.warn, anim(ui, Id::unique("clip-est-over"), over, 0.3));
            let painter = ui.painter();
            painter.text(est.left_top(), Align2::LEFT_TOP, format!("≈ {}", export::size_label(bytes)), semibold(26.0), color);
            let tracks = src.info.tracks.len();
            let sub = format!("{} of {} · {}", fmt_short(job.len()), fmt_short(src.info.duration), sound_line(job, tracks));
            painter.text(est.left_bottom(), Align2::LEFT_BOTTOM, sub, sans(12.0), p.t3);
        }
        let gif = self.saved.export.format == Format::Gif;
        self.save_button(ui, look, save_row, gif);
    }

    fn rows(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let Some((size, src_fps)) = self.info().map(|i| (i.size, i.fps)) else { return };
        let width = ui.available_width();
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.label(RichText::new("export").font(semibold(15.0)).color(p.t1));
        gap(ui);

        let s = &mut self.saved.export;
        label(ui, look, "as");
        let formats = [(Format::Video, "video"), (Format::Gif, "gif")];
        widgets::segmented(ui, look, &mut s.format, &formats, Some(width));
        gap(ui);
        let video = s.format == Format::Video;

        label(ui, look, "size");
        let (custom, custom_mbps) = (&mut self.custom_mb, &mut self.custom_mbps);
        let compressing = video && s.compress.is_some();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            if pill(ui, look, "original", s.target_mb.is_none() && custom.is_empty() && !compressing).clicked() {
                s.set_target(None);
                s.set_compress(None);
                custom.clear();
            }
            if video
                && pill(ui, look, "compress", compressing)
                    .on_hover_text("re-encodes at one steady quality: much smaller and hard to tell apart; how much smaller depends on the video")
                    .clicked()
                && !compressing
            {
                s.set_compress(Some(Compress::default()));
                custom.clear();
                custom_mbps.clear();
            }
            for mb in TARGETS_MB {
                if pill(ui, look, &format!("≤ {mb} MB"), s.target_mb == Some(mb) && custom.is_empty()).clicked() {
                    s.set_target(Some(mb));
                    custom.clear();
                    custom_mbps.clear();
                }
            }
            let resp = pill_field(ui, look, custom, "… MB", "size limit in MB", 64.0, false).on_hover_text("your own limit in MB");
            if resp.changed() {
                custom.retain(|c| c.is_ascii_digit());
                s.set_target(custom.parse::<u32>().ok().filter(|&mb| mb > 0));
                if s.target_mb.is_some() {
                    custom_mbps.clear();
                }
            }
        });
        if compressing && let Some(level) = &mut s.compress {
            gap(ui);
            label(ui, look, "compression");
            let levels = [(Compress::Light, "light"), (Compress::Medium, "medium"), (Compress::Strong, "strong")];
            widgets::segmented(ui, look, level, &levels, Some(width))
                .on_hover_text("stronger is smaller and loses a little more detail in busy scenes");
        }
        gap(ui);

        let crop = self.edit.crop.filter(|c| !c.is_full(size)).map(|c| c.even(size));
        let base = export::base_size(size, crop);
        let labels = |h: Option<u32>| {
            let (w, h) = export::scaled(base, h);
            if crop.is_some() { format!("{w}×{h}") } else { format!("{h}p") }
        };
        let heights = export::heights(base);
        let chosen = s.height.filter(|h| heights.contains(h));
        label(ui, look, "resolution");
        let mut items = vec![format!("{} (original)", labels(None))];
        items.extend(heights.iter().map(|&h| labels(Some(h))));
        let current = match chosen {
            Some(h) => labels(Some(h)),
            None => items[0].clone(),
        };
        if let Some(i) = widgets::dropdown(ui, look, &current, &items, width) {
            s.height = i.checked_sub(1).map(|i| heights[i]);
        }

        if s.format == Format::Gif {
            gap(ui);
            label(ui, look, "frame rate");
            let items: Vec<String> = GIF_FPS.iter().map(|f| format!("{f} fps")).collect();
            if let Some(i) = widgets::dropdown(ui, look, &format!("{} fps", s.gif_fps), &items, width) {
                s.gif_fps = GIF_FPS[i];
            }
        } else {
            let rates = export::frame_rates(src_fps);
            if !rates.is_empty() {
                gap(ui);
                label(ui, look, "frame rate");
                let original = format!("{} (original)", fps_label(src_fps));
                let mut items = vec![original.clone()];
                items.extend(rates.iter().map(|&f| format!("{f} fps")));
                let rows: Vec<(&str, &str)> = items.iter().map(|i| (i.as_str(), "")).collect();
                let current = s.fps.filter(|f| rates.contains(f)).map_or(original, |f| format!("{f} fps"));
                let d = widgets::dropdown_detail(ui, look, &current, &rows, width, width);
                ui.ctx().accesskit_node_builder(d.response.id, |n| n.set_label("frame rate"));
                d.response.on_hover_text("fewer frames per second make a smaller file; the timing stays exact");
                if let Some(i) = d.picked {
                    s.fps = i.checked_sub(1).map(|i| rates[i]);
                }
            }
        }
        let unmuted = self.edit.lanes.iter().filter(|l| !l.muted && l.source.is_some()).count();
        if s.format == Format::Video && unmuted > 1 {
            gap(ui);
            widgets::checkbox(ui, look, &mut s.combine, "combine sound into one track")
                .on_hover_text("mixes the sound you hear into a single track, for players that only play the first one");
        }
        if let Some(c) = crop {
            gap(ui);
            self.crop_row(ui, look, c);
        }
        if self.edit.clicks.is_some() {
            gap(ui);
            self.clicks_row(ui, look);
        }
        if self.edit.keys.is_some() {
            gap(ui);
            self.keys_row(ui, look);
        }
        if video {
            gap(ui);
            self.more_options(ui, look);
        }
    }

    /// The "more options" disclosure: bitrate and codec, sliding open under its header.
    fn more_options(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let width = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(vec2(width, 22.0), Sense::click());
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            self.saved.more = !self.saved.more;
        }
        let open = self.saved.more;
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "more options"));
        ui.ctx().accesskit_node_builder(resp.id, |n| n.set_expanded(open));
        let t = ease(anim(ui, Id::unique("clip-more-open"), open, 0.22));
        let h = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
        let color = mix(p.t2, p.t1, h);
        let c = pos2(r.left() + 5.0, r.center().y);
        let turn = std::f32::consts::FRAC_PI_2 * t;
        let point = |x: f32, y: f32| c + vec2(x * turn.cos() - y * turn.sin(), x * turn.sin() + y * turn.cos());
        ui.painter().add(Shape::line(vec![point(-2.0, -4.0), point(2.0, 0.0), point(-2.0, 4.0)], Stroke::new(1.5, color)));
        ui.painter().text(pos2(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, "more options", sans(13.0), color);
        if t <= 0.0 {
            return;
        }
        let height_id = Id::unique("clip-more-height");
        let full = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(140.0);
        let at = ui.cursor().min + vec2(0.0, 6.0);
        let shown = Rect::from_min_size(at, vec2(width, full * t));
        let mut body = ui.new_child(
            UiBuilder::new().max_rect(Rect::from_min_size(at, vec2(width, 2000.0))).layout(Layout::top_down(Align::Min)),
        );
        body.set_clip_rect(shown.intersect(ui.clip_rect()));
        body.set_opacity(t);
        body.spacing_mut().item_spacing = vec2(6.0, 6.0);
        self.more_rows(&mut body, look);
        let measured = body.min_rect().height();
        ui.data_mut(|d| d.insert_temp(height_id, measured));
        ui.advance_cursor_after_rect(shown);
        if t < 1.0 {
            ui.ctx().request_repaint();
        }
    }

    fn more_rows(&mut self, ui: &mut Ui, look: &Look) {
        let p = look.p;
        let width = ui.available_width();
        let source = self.info().and_then(|i| i.codec);
        let effective = match self.job() {
            Some(Job::Video(VideoJob { video: Video::Encode { bitrate, .. }, .. })) => Some(bitrate),
            _ => None,
        };
        let s = &mut self.saved.export;
        label(ui, look, "bitrate");
        let (text, sizes) = (&mut self.custom_mbps, &mut self.custom_mb);
        let warn = !text.is_empty() && export::parse_mbps(text).is_none();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
            let auto = s.kbps.is_none() && (text.is_empty() || warn);
            if pill(ui, look, "auto", auto)
                .on_hover_text("from the size limit when one is set, otherwise about the source's quality")
                .clicked()
            {
                s.set_kbps(None);
                text.clear();
            }
            let resp = pill_field(ui, look, text, "… Mbps", "bitrate in Mbps", 72.0, warn)
                .on_hover_text("your own video bitrate, 0.2 to 200 Mbps; replaces the size limit");
            if resp.changed() {
                text.retain(|c| c.is_ascii_digit() || c == '.' || c == ',');
                s.set_kbps(export::parse_mbps(text));
                if s.kbps.is_some() {
                    sizes.clear();
                }
            }
            if let (None, Some(_), Some(bps)) = (s.kbps, s.target_mb, effective) {
                ui.label(RichText::new(format!("· {}", mbps(bps))).font(sans(12.0)).color(p.t3));
            }
        });
        gap(ui);

        label(ui, look, "codec");
        let mut choices = vec![(CodecChoice::Same, format!("same as source ({})", codec_name(source.unwrap_or(Codec::H264))))];
        choices.push((CodecChoice::H264, "h.264".into()));
        if self.hevc || s.codec == CodecChoice::Hevc {
            choices.push((CodecChoice::Hevc, "hevc".into()));
        }
        let rows: Vec<(&str, &str)> = choices.iter().map(|(_, n)| (n.as_str(), "")).collect();
        let current = choices.iter().find(|(c, _)| *c == s.codec).map_or(choices[0].1.clone(), |(_, n)| n.clone());
        let d = widgets::dropdown_detail(ui, look, &current, &rows, width, width);
        ui.ctx().accesskit_node_builder(d.response.id, |n| n.set_label("codec"));
        d.response.on_hover_text(HEVC_TIP);
        if let Some(i) = d.picked {
            s.codec = choices[i].0;
        }
    }

    fn clicks_row(&mut self, ui: &mut Ui, look: &Look) {
        let Some(mut shown) = self.edit.clicks.as_ref().map(|c| c.shown) else { return };
        let width = ui.available_width();
        label(ui, look, "clicks");
        let toggled = widgets::checkbox(ui, look, &mut shown, "show clicks")
            .on_hover_text("draws a ripple on every click into the file; hidden clicks aren't in it at all")
            .changed();
        if toggled {
            self.change(|e| e.clicks.as_mut().map(|c| c.shown = shown).is_some());
        }
        let look_ = &mut self.saved.export.clicks;
        let sizes = [(ClickSize::Small, "small"), (ClickSize::Normal, "normal"), (ClickSize::Large, "large")];
        widgets::segmented(ui, look, &mut look_.size, &sizes, Some(width));
        let accent = Color32::from_rgb(self.accent[0], self.accent[1], self.accent[2]);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
            let mut choices = vec![(ClickColor::Accent, accent, "accent"), (ClickColor::White, Color32::WHITE, "white")];
            choices.extend(CLICK_COLORS.iter().map(|&(rgb, name)| (ClickColor::Custom(rgb), Color32::from_rgb(rgb[0], rgb[1], rgb[2]), name)));
            for (choice, color, name) in choices {
                let resp = widgets::swatch(ui, look, color, look_.color == choice).on_hover_text(name);
                resp.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, look_.color == choice, format!("click colour: {name}")));
                if resp.clicked() {
                    look_.color = choice;
                }
            }
        });
    }

    fn keys_row(&mut self, ui: &mut Ui, look: &Look) {
        let Some(mut shown) = self.edit.keys.as_ref().map(|k| k.shown) else { return };
        let width = ui.available_width();
        label(ui, look, "keys");
        let toggled = widgets::checkbox(ui, look, &mut shown, "show keys")
            .on_hover_text("draws the keys you pressed into the file; hidden keys aren't in it at all")
            .changed();
        if toggled {
            self.change(|e| e.keys.as_mut().map(|k| k.shown = shown).is_some());
        }
        let look_ = &mut self.saved.export.keys;
        let places = [(Position::Left, "left"), (Position::Centre, "centre"), (Position::Right, "right")];
        widgets::segmented(ui, look, &mut look_.pos, &places, Some(width));
        let sizes = [(KeySize::Small, "small"), (KeySize::Normal, "normal"), (KeySize::Large, "large")];
        widgets::segmented(ui, look, &mut look_.size, &sizes, Some(width));
    }

    fn crop_row(&mut self, ui: &mut Ui, look: &Look, c: Crop) {
        let p = look.p;
        label(ui, look, "crop");
        let mut reset = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 0.0);
            ui.label(RichText::new(format!("{}×{}", c.w, c.h)).font(mono(12.0)).color(p.t1));
            ui.label(RichText::new("·").font(sans(13.0)).color(p.t3));
            reset = widgets::link(ui, look, "reset").clicked();
        });
        if reset {
            self.cropping = None;
            self.change(|e| {
                e.crop = None;
                true
            });
        }
    }

    fn save_button(&mut self, ui: &mut Ui, look: &Look, row: Rect, gif: bool) {
        let p = look.p;
        self.save_rect = row;
        let go = Rect::from_min_max(row.min, pos2(row.right() - MORE_W, row.bottom()));
        let more = Rect::from_min_max(pos2(go.right(), row.top()), row.max);
        let ready = self.info.is_ok();
        let go_resp = ui.interact(go, Id::unique("clip-save"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        let more_resp = ui
            .interact(more, Id::unique("clip-more"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("more ways to save");
        let since = self.now - self.flash;
        let progress = self.task.as_ref().map(|t| t.permille.load(Ordering::Relaxed) as f32 / 1000.0);
        let label = match (&self.task, &self.upload) {
            (Some(_), _) if go_resp.hovered() => "cancel".to_string(),
            (Some(t), _) if t.copy => "saving…".to_string(),
            (Some(_), _) => format!("saving {} %", (progress.unwrap_or(0.0) * 100.0).round()),
            (None, Some(u)) => format!("uploading to {}…", u.host),
            _ if since < super::SAVED_FLASH => "saved ✓".to_string(),
            _ if gif => "save gif".to_string(),
            _ => "save".to_string(),
        };
        if since < super::SAVED_FLASH {
            ui.ctx().request_repaint();
        }
        go_resp.widget_info(|| WidgetInfo::labeled(Role::Button, ready, &label));
        more_resp.widget_info(|| WidgetInfo::labeled(Role::Button, ready, "more ways to save"));
        let h = anim(ui, go_resp.id.with("hov"), go_resp.hovered(), 0.12);
        let hm = anim(ui, more_resp.id.with("hov"), more_resp.hovered() || self.menu, 0.12);
        let d = anim(ui, go_resp.id.with("prs"), go_resp.is_pointer_button_down_on(), 0.08);
        let painter = ui.painter();
        let left = egui::CornerRadius { nw: 8, sw: 8, ne: 0, se: 0 };
        let right = egui::CornerRadius { nw: 0, sw: 0, ne: 8, se: 8 };
        painter.rect_filled(go, left, mix(mix(p.acc, p.acch, h), p.accp, d));
        painter.rect_filled(more, right, mix(p.acc, p.acch, hm));
        painter.vline(more.left(), more.y_range(), Stroke::new(1.0, Color32::from_black_alpha(50)));
        let target = match (progress, since < super::SAVED_FLASH) {
            (Some(f), _) => f,
            (None, true) => 1.0,
            _ => 0.0,
        };
        let fill = glide(ui, Id::unique("clip-save-fill"), target, if target == 0.0 { 0.0 } else { 0.1 });
        let fade = if self.task.is_none() && since >= super::SAVED_FLASH { 0.0 } else { 1.0 };
        if fill > 0.0 && fade > 0.0 {
            let r = Rect::from_min_size(go.min, vec2(go.width() * fill, go.height()));
            painter.rect_filled(r, left, Color32::from_white_alpha(56));
        }
        painter.text(go.center(), Align2::CENTER_CENTER, &label, semibold(15.0), p.on_acc);
        let c = more.center();
        let chevron = vec![c + vec2(-4.5, -2.0), c + vec2(0.0, 2.5), c + vec2(4.5, -2.0)];
        painter.add(Shape::line(chevron, Stroke::new(1.6, p.on_acc)));
        if go_resp.clicked() && ready {
            match &self.task {
                Some(t) => t.cancel.store(true, Ordering::Relaxed),
                None if self.upload.is_none() => self.save(),
                None => {}
            }
        }
        if more_resp.clicked() {
            self.menu = !self.menu;
            self.sheet = false;
        }
    }
}
