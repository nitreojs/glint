//! The editor without a file, and files arriving: a drop zone with an open button and the newest
//! recordings as quick picks; a video dropped anywhere, any time, opens in place. Edits left
//! unsaved on the file it replaces stay one click away ("back").

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CursorIcon, Id, Layout, Order, Pos2, Rect, Role, Sense, Shape, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, UiBuilder, WidgetInfo, emath::TSTransform, pos2, vec2,
};
use glint_core::clip::edit::Edit;
use glint_core::clip::glide::Glide;
use glint_core::clip::math::fmt_short;
use glint_core::gallery::{Item, Maker, path_id};
use glint_media::clip::media::Com;
use glint_ui::widgets::{self, Kind, Look, anim, ease, mix, mono, sans, semibold};

use super::preview::GLASS;
use super::{ClipApp, LETTERBOX, Note, Opened, is_video, probe, title};

/// How many recordings the start screen offers at most.
const RECENT: usize = 5;
const CARD_MIN_W: f32 = 150.0;
const CARD_GAP: f32 = 12.0;
const ZONE_MAX_W: f32 = 720.0;
const ONLY_VIDEOS: &str = "opens mp4, mov, m4v and mkv";

/// The start screen's newest recordings, read off the UI thread.
#[derive(Default)]
pub(super) struct Start {
    loading: Option<Receiver<Loaded>>,
    cards: Vec<Card>,
}

struct Card {
    path: PathBuf,
    name: String,
    length_ms: Option<u32>,
    thumb: Option<TextureHandle>,
}

enum Loaded {
    Items(Vec<Item>),
    Thumb(usize, ColorImage),
}

impl Start {
    /// Starts reading the library, once.
    fn load(&mut self, ctx: &egui::Context) {
        if self.loading.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _com = Com::init();
            let items = glint_ui::gallery::recent_recordings(Maker::Glint, RECENT);
            if tx.send(Loaded::Items(items.clone())).is_err() {
                return;
            }
            ctx.request_repaint();
            for (i, item) in items.iter().enumerate() {
                let Some(image) = glint_ui::gallery::thumbnail(Maker::Glint.name(), item) else { continue };
                if tx.send(Loaded::Thumb(i, image)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
        self.loading = Some(rx);
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.loading else { return };
        while let Ok(got) = rx.try_recv() {
            match got {
                Loaded::Items(items) => {
                    self.cards = items
                        .into_iter()
                        .map(|i| Card { name: i.name(), length_ms: i.duration_ms, path: i.path, thumb: None })
                        .collect();
                }
                Loaded::Thumb(i, image) => {
                    if let Some(c) = self.cards.get_mut(i) {
                        c.thumb = Some(ctx.load_texture(format!("clip-recent-{i}"), image, TextureOptions::LINEAR));
                    }
                }
            }
        }
    }
}

/// A file being read to open, with the edits to put back on it ("back").
pub(super) struct Opening {
    path: PathBuf,
    restore: Option<Edit>,
    thread: JoinHandle<Opened>,
}

/// A file held over the window.
pub(super) struct Held {
    path: PathBuf,
    video: bool,
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A rounded rectangle's outline, closed.
fn outline(r: Rect, radius: f32) -> Vec<Pos2> {
    let corners = [
        (pos2(r.right() - radius, r.top() + radius), -90.0_f32),
        (pos2(r.right() - radius, r.bottom() - radius), 0.0),
        (pos2(r.left() + radius, r.bottom() - radius), 90.0),
        (pos2(r.left() + radius, r.top() + radius), 180.0),
    ];
    let mut points: Vec<Pos2> = corners
        .iter()
        .flat_map(|&(c, from)| {
            (0..=8).map(move |k| {
                let a = (from + 90.0 * k as f32 / 8.0).to_radians();
                c + vec2(a.cos(), a.sin()) * radius
            })
        })
        .collect();
    points.push(points[0]);
    points
}

/// An arrow going down into a tray, centred on `c`; `drop` lowers the arrow.
fn drop_icon(painter: &egui::Painter, c: Pos2, color: Color32, drop: f32) {
    let stroke = Stroke::new(1.8, color);
    let tray = vec![c + vec2(-15.0, 3.0), c + vec2(-15.0, 12.0), c + vec2(15.0, 12.0), c + vec2(15.0, 3.0)];
    painter.add(Shape::line(tray, stroke));
    let a = c + vec2(0.0, 3.0 * drop);
    painter.line_segment([a + vec2(0.0, -15.0), a + vec2(0.0, 4.0)], stroke);
    painter.add(Shape::line(vec![a + vec2(-6.0, -2.0), a + vec2(0.0, 4.0), a + vec2(6.0, -2.0)], stroke));
}

impl ClipApp {
    /// The file held over the window, if any (the first video of those held); a dropped video
    /// opens.
    pub(super) fn files(&mut self, ctx: &egui::Context) -> Option<Held> {
        let (held, dropped): (Vec<PathBuf>, Vec<PathBuf>) = ctx.input(|i| {
            (
                i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect(),
                i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect(),
            )
        });
        if !dropped.is_empty() {
            match dropped.into_iter().find(|p| is_video(p)) {
                Some(path) => self.open(path, None),
                None => self.toast(Note::Info(ONLY_VIDEOS.into())),
            }
        }
        let path = held.iter().find(|p| is_video(p)).or(held.first()).cloned()?;
        Some(Held { video: is_video(&path), path })
    }

    /// Asks for a video and opens it.
    pub(super) fn browse(&mut self) {
        self.pause();
        let folder = self.path.parent().map(Path::to_path_buf);
        if let Some(path) = super::actions::open_dialog(self.owner(), folder.as_deref()) {
            self.open(path, None);
        }
    }

    /// Starts reading `path` to open it in place of what's open; `restore` puts edits back on it.
    pub(super) fn open(&mut self, path: PathBuf, restore: Option<Edit>) {
        if !is_video(&path) {
            return self.toast(Note::Info(ONLY_VIDEOS.into()));
        }
        if self.busy() {
            return self.toast(Note::Info("still saving: open it once that's done".into()));
        }
        if self.start.is_none() && restore.is_none() && path_id(&path) == path_id(&self.path) {
            return;
        }
        let reading = path.clone();
        let thread = std::thread::spawn(move || {
            let _com = Com::init();
            probe(reading)
        });
        self.opening = Some(Opening { path, restore, thread });
    }

    /// Opens what [`ClipApp::open`] read, once it's read.
    pub(super) fn poll_opening(&mut self, ctx: &egui::Context) {
        let Some(o) = &self.opening else { return };
        if !o.thread.is_finished() {
            ctx.request_repaint_after(Duration::from_millis(30));
            return;
        }
        let Some(Opening { path, restore, thread }) = self.opening.take() else { return };
        let name = name_of(&path);
        let opened = thread.join().unwrap_or_else(|_| Opened { path, info: Err("reading it crashed".into()), index: None });
        match &opened.info {
            Ok(_) => self.swap_in(ctx, opened, restore),
            Err(e) => self.toast(Note::Failed(format!("can't open {name}: {e}"))),
        }
    }

    /// Puts `opened` in the window in place of what's there, the window, settings and export
    /// choices staying; edits not saved on the file it replaces are kept for "back".
    pub(super) fn swap_in(&mut self, ctx: &egui::Context, opened: Opened, restore: Option<Edit>) {
        let left = (self.start.is_none() && self.edit != self.pristine).then(|| (self.path.clone(), self.edit.clone(), self.name.clone()));
        let mut fresh = ClipApp::new(Some(opened), self.effort, self.host.clone(), self.saved.clone(), self.accent);
        (fresh.hwnd, fresh.live, fresh.hevc, fresh.light, fresh.keyboard, fresh.now) =
            (self.hwnd, self.live, self.hevc, self.light, self.keyboard, self.now);
        if let Some(edit) = restore {
            fresh.trim = [Glide::new(edit.range.start), Glide::new(edit.range.end)];
            fresh.edit = edit;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title(&fresh.name)));
        self.live = false;
        std::mem::replace(self, fresh).retire();
        if let Some((path, edit, name)) = left {
            self.previous = Some((path, edit));
            self.toast(Note::Left(name));
        }
    }

    /// Lets a replaced file's players stop on a thread of their own, so the swap doesn't wait.
    fn retire(mut self) {
        let media = (self.player.take(), self.audio.take(), self.scan.take(), self.waves.take());
        std::thread::spawn(move || drop(media));
    }

    /// Back to the file left with unsaved edits, edits and all.
    pub(super) fn back(&mut self) {
        if let Some((path, edit)) = self.previous.take() {
            self.open(path, Some(edit));
        }
    }

    pub(super) fn start_screen(&mut self, ui: &mut Ui, look: &Look, body: Rect, held: Option<&Held>) {
        let ctx = ui.ctx().clone();
        if let Some(s) = &mut self.start {
            if self.live {
                s.load(&ctx);
            }
            s.poll(&ctx);
        }
        let count = self.start.as_ref().map_or(0, |s| s.cards.len());
        let width = (body.width() - 112.0).min(ZONE_MAX_W);
        let zone_h = (body.height() * 0.42).clamp(200.0, 300.0);
        let fit = ((width + CARD_GAP) / (CARD_MIN_W + CARD_GAP)).floor() as usize;
        let n = count.min(fit);
        let card_w = if n > 0 { (width - (n - 1) as f32 * CARD_GAP) / n as f32 } else { 0.0 };
        let thumb_h = card_w * 9.0 / 16.0;
        let recent_h = if n > 0 { 28.0 + 26.0 + thumb_h + 26.0 } else { 0.0 };
        // the zone slides up to make room when the recordings arrive
        let room = widgets::glide(ui, Id::unique("clip-start-room"), recent_h, 0.25);
        let top = body.center().y - (zone_h + room) / 2.0;
        let zone = Rect::from_min_size(pos2(body.center().x - width / 2.0, top), vec2(width, zone_h));
        self.drop_zone(ui, look, zone, held);
        let shown = ease(anim(ui, Id::unique("clip-recent-shown"), n > 0, 0.3));
        if n > 0 {
            self.recent(ui, look, pos2(zone.left(), zone.bottom() + 28.0), n, vec2(card_w, thumb_h), shown);
        }
    }

    fn drop_zone(&mut self, ui: &mut Ui, look: &Look, zone: Rect, held: Option<&Held>) {
        let p = look.p;
        let resp = ui.interact(zone, Id::unique("clip-drop-zone"), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "open a video"));
        let hov = anim(ui, resp.id.with("hov"), resp.hovered(), 0.12);
        let t = ease(anim(ui, resp.id.with("held"), held.is_some(), 0.18));
        let ok = anim(ui, resp.id.with("ok"), held.is_none_or(|h| h.video), 0.18);
        let lit = t * ok;
        let c = zone.center();
        let r = Rect::from_center_size(c, zone.size() * (1.0 + 0.015 * lit));
        let painter = ui.painter();
        painter.rect_filled(r, 14, mix(p.field, mix(p.field, p.acc, 0.12), lit));
        let edge = mix(mix(p.line2, p.line3, hov), p.t3, t * (1.0 - ok));
        if t < 1.0 {
            let dashes = Shape::dashed_line(&outline(r.shrink(0.5), 14.0), Stroke::new(1.2, edge.gamma_multiply(1.0 - lit)), 7.0, 5.0);
            painter.extend(dashes);
        }
        if lit > 0.0 {
            painter.rect_stroke(r, 14, Stroke::new(2.0, p.acc.gamma_multiply(lit)), StrokeKind::Inside);
        }
        drop_icon(painter, c + vec2(0.0, -46.0), mix(mix(p.t3, p.t2, hov), p.acc, lit), lit);
        let opening = self.opening.as_ref().map(|o| name_of(&o.path));
        let (title, sub) = match (held, opening) {
            (Some(h), _) => (if h.video { "drop to open" } else { "not a video" }, name_of(&h.path)),
            (None, Some(name)) => ("opening…", name),
            (None, None) => ("drop a video", "mp4 · mov · m4v · mkv".to_string()),
        };
        painter.text(c + vec2(0.0, -2.0), Align2::CENTER_CENTER, title, semibold(18.0), p.t1);
        let sub_w = zone.width() - 48.0;
        let job = egui::text::LayoutJob {
            wrap: egui::text::TextWrapping::truncate_at_width(sub_w),
            halign: Align::Center,
            ..egui::text::LayoutJob::single_section(sub, egui::TextFormat::simple(sans(12.0), p.t3))
        };
        let g = painter.layout_job(job);
        painter.galley(pos2(c.x, c.y + 22.0 - g.size().y / 2.0), g, p.t3);
        let mut browse = resp.clicked();
        if held.is_none() && self.opening.is_none() {
            let row = Rect::from_center_size(c + vec2(0.0, 62.0), vec2(zone.width(), 30.0));
            ui.scope_builder(UiBuilder::new().max_rect(row).layout(Layout::top_down(Align::Center)), |ui| {
                browse |= widgets::button(ui, look, "open file…", Kind::Secondary, 30.0, true).clicked();
            });
        }
        if browse {
            self.browse();
        }
    }

    /// The newest recordings, `n` cards of `size` (the picture's) from `at`, `shown` faded in; a
    /// click opens one.
    fn recent(&mut self, ui: &mut Ui, look: &Look, at: Pos2, n: usize, size: egui::Vec2, shown: f32) {
        let p = look.p;
        let mut painter = ui.painter().clone();
        painter.set_opacity(shown);
        painter.text(at, Align2::LEFT_TOP, "recent", sans(12.0), p.t3);
        let mut picked = None;
        let Some(start) = &self.start else { return };
        for (i, card) in start.cards.iter().take(n).enumerate() {
            let x = at.x + i as f32 * (size.x + CARD_GAP);
            let pic = Rect::from_min_size(pos2(x, at.y + 26.0), size);
            let whole = Rect::from_min_max(pic.min, pic.max + vec2(0.0, 26.0));
            let resp = ui.interact(whole, Id::unique(("clip-recent", i)), Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, &card.name));
            let h = ease(anim(ui, resp.id.with("hov"), resp.hovered(), 0.12));
            let pic = pic.translate(vec2(0.0, -2.0 * h));
            painter.rect_filled(pic, 6, LETTERBOX);
            let fade = ease(anim(ui, resp.id.with("thumb"), card.thumb.is_some(), 0.25)) * shown;
            if let Some(tex) = &card.thumb {
                let [w, hgt] = tex.size().map(|v| v.max(1) as f32);
                let (want, have) = (size.x / size.y, w / hgt);
                let uv = if have > want {
                    let u = want / have;
                    Rect::from_min_max(pos2((1.0 - u) / 2.0, 0.0), pos2((1.0 + u) / 2.0, 1.0))
                } else {
                    let v = have / want;
                    Rect::from_min_max(pos2(0.0, (1.0 - v) / 2.0), pos2(1.0, (1.0 + v) / 2.0))
                };
                egui::Image::from_texture((tex.id(), tex.size_vec2()))
                    .uv(uv)
                    .corner_radius(6)
                    .tint(Color32::WHITE.gamma_multiply(fade))
                    .paint_at(ui, pic);
            }
            painter.rect_stroke(pic, 6, Stroke::new(1.0 + 0.5 * h, mix(p.line2, p.acc, h)), StrokeKind::Inside);
            if let Some(ms) = card.length_ms {
                let g = painter.layout_no_wrap(fmt_short(ms as i64 * 10_000), mono(11.0), Color32::WHITE);
                let pill = Rect::from_min_size(pic.right_bottom() - g.size() - vec2(14.0, 10.0), g.size() + vec2(8.0, 4.0));
                painter.rect_filled(pill, 4, GLASS);
                painter.galley(pill.min + vec2(4.0, 2.0), g, Color32::WHITE);
            }
            let job = egui::text::LayoutJob {
                wrap: egui::text::TextWrapping::truncate_at_width(size.x),
                ..egui::text::LayoutJob::single_section(card.name.clone(), egui::TextFormat::simple(sans(12.0), p.t2))
            };
            let g = painter.layout_job(job);
            painter.galley(pos2(x, pic.bottom() + 8.0), g, mix(p.t2, p.t1, h));
            if resp.clicked() {
                picked = Some(card.path.clone());
            }
        }
        if let Some(path) = picked {
            self.open(path, None);
        }
    }

    /// Over an open clip: what a file held over the window would do, then that it's opening.
    pub(super) fn drop_overlay(&mut self, ctx: &egui::Context, look: &Look, body: Rect, held: Option<&Held>) {
        let p = look.p;
        let opening = self.opening.as_ref().map(|o| name_of(&o.path));
        let show = held.is_some() || opening.is_some();
        let t = ease(widgets::anim_ctx(ctx, Id::unique("clip-drop-shown"), show, 0.18));
        let said = Id::unique("clip-drop-said");
        let (title, sub, video) = match (held, opening) {
            (Some(h), _) => (if h.video { "drop to open" } else { "not a video" }, name_of(&h.path), h.video),
            (None, Some(name)) => ("opening…", name, true),
            // fading out: what it last said
            (None, None) => ctx.data(|d| d.get_temp::<(&'static str, String, bool)>(said)).unwrap_or(("", String::new(), true)),
        };
        if t <= 0.0 {
            return;
        }
        ctx.data_mut(|d| d.insert_temp(said, (title, sub.clone(), video)));
        let ok = widgets::anim_ctx(ctx, Id::unique("clip-drop-ok"), video, 0.18);
        let area = egui::Area::new(Id::unique("clip-drop")).order(Order::Foreground).fixed_pos(body.min).interactable(false);
        let shown = area.show(ctx, |ui| {
            ui.set_opacity(t);
            let painter = ui.painter();
            let r = glint_ui::window::radius(ctx);
            let corners = egui::CornerRadius { nw: 0, ne: 0, sw: r, se: r };
            let scrim = if self.light == Some(true) { Color32::from_white_alpha(225) } else { Color32::from_rgba_premultiplied(12, 12, 12, 225) };
            painter.rect_filled(body, corners, scrim);
            let frame = body.shrink(18.0);
            painter.rect(frame, 14, p.acc.gamma_multiply(0.08 * ok), Stroke::new(2.0, mix(p.line3, p.acc, ok)), StrokeKind::Inside);
            let c = frame.center();
            drop_icon(painter, c + vec2(0.0, -44.0), mix(p.t3, p.acc, ok), t * ok);
            painter.text(c, Align2::CENTER_CENTER, title, semibold(20.0), p.t1);
            let job = egui::text::LayoutJob {
                wrap: egui::text::TextWrapping::truncate_at_width(frame.width() - 80.0),
                halign: Align::Center,
                ..egui::text::LayoutJob::single_section(sub, egui::TextFormat::simple(sans(13.0), p.t3))
            };
            let g = painter.layout_job(job);
            painter.galley(pos2(c.x, c.y + 26.0 - g.size().y / 2.0), g, p.t3);
        });
        let pivot = body.center().to_vec2();
        let s = 0.98 + 0.02 * t;
        ctx.set_transform_layer(shown.response.layer_id, TSTransform::from_translation(pivot) * TSTransform::from_scaling(s) * TSTransform::from_translation(-pivot));
    }
}
